//! Standalone F2-01 capability probe; not an extraction implementation.
//! Run from src-tauri: cargo run --example archive_spike -- ../contract-fixtures/archive-spike
use sevenz_rust2::{
    ArchiveEntry, ArchiveReader, ArchiveWriter, EncoderMethod, Password,
    encoder_options::AesEncoderOptions,
};
use std::{
    error::Error,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;
const TEXT: &[u8] = "Ferrum-owned fixture: café/東京\n".as_bytes();
const SEVEN_SIGNATURE: &[u8; 6] = b"7z\xBC\xAF\x27\x1C";
const PASSWORD: &str = "fixture-only-not-a-secret";
// unrar_sys 0.5.8 omits Advapi32 from its Windows link directives.
#[cfg(windows)]
#[link(name = "Advapi32")]
unsafe extern "system" {}

fn copy_count(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    cancelled: &AtomicBool,
) -> io::Result<u64> {
    let mut buf = [0u8; 8192];
    let mut done = 0;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(done);
        }
        writer.write_all(&buf[..n])?;
        done += n as u64;
    }
}

struct StopAfterFirstWrite<'a> {
    cancelled: &'a AtomicBool,
    bytes: usize,
}

impl Write for StopAfterFirstWrite<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes += bytes.len();
        self.cancelled.store(true, Ordering::Relaxed);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn zip_create(path: &Path, name: &str, data: &[u8], encrypted: bool) -> Result<()> {
    let mut writer = ZipWriter::new(File::create(path)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    if encrypted {
        writer.start_file(
            name,
            options.with_aes_encryption(zip::AesMode::Aes256, PASSWORD),
        )?;
    } else {
        writer.start_file(name, options)?;
    }
    io::copy(&mut &data[..], &mut writer)?;
    writer.finish()?;
    Ok(())
}

fn zip_large(path: &Path) -> Result<()> {
    let mut writer = ZipWriter::new(File::create(path)?);
    writer.start_file(
        "repeat.bin",
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )?;
    assert_eq!(
        io::copy(&mut io::repeat(b'A').take(1024 * 1024), &mut writer)?,
        1024 * 1024
    );
    writer.finish()?;
    Ok(())
}

fn check_large(path: &Path) -> Result<()> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    assert_eq!(archive.len(), 1);
    let mut entry = archive.by_index(0)?;
    assert_eq!(entry.name(), "repeat.bin");
    let mut buf = [0; 8192];
    let mut total = 0;
    loop {
        let n = entry.read(&mut buf)?;
        if n == 0 {
            break;
        }
        assert!(buf[..n].iter().all(|byte| *byte == b'A'));
        total += n;
    }
    assert_eq!(total, 1024 * 1024);
    println!("ZIP 1 MiB streaming create/read: {} bytes", total);
    Ok(())
}

fn zip_check(path: &Path, name: &str, expected: &[u8], password: bool) -> Result<()> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    assert_eq!(archive.len(), 1);
    let mut member = if password {
        archive.by_index_decrypt(0, PASSWORD.as_bytes())?
    } else {
        archive.by_index(0)?
    };
    assert_eq!(member.name(), name);
    let output = path.with_extension("out");
    let bytes = copy_count(
        &mut member,
        &mut File::options().write(true).create_new(true).open(&output)?,
        &AtomicBool::new(false),
    )?;
    assert_eq!(bytes as usize, expected.len());
    assert_eq!(fs::read(&output)?, expected);
    fs::remove_file(output)?;
    println!("ZIP list/extract/test {}: {} bytes consumed", name, bytes);
    Ok(())
}

fn seven_create(path: &Path, name: &str, data: &[u8], encrypted: bool) -> Result<()> {
    let mut writer = ArchiveWriter::create(path)?;
    if encrypted {
        writer.set_content_methods(vec![
            EncoderMethod::LZMA2.into(),
            AesEncoderOptions::new(Password::from(PASSWORD)).into(),
        ]);
    }
    writer.push_archive_entry(ArchiveEntry::new_file(name), Some(data))?;
    writer.finish()?;
    Ok(())
}

fn seven_check(path: &Path, name: &str, expected: &[u8], password: bool) -> Result<()> {
    let mut reader = ArchiveReader::open(
        path,
        if password {
            Password::from(PASSWORD)
        } else {
            Password::empty()
        },
    )?;
    assert_eq!(reader.archive().files.len(), 1);
    assert_eq!(reader.archive().files[0].name(), name);
    let mut visited = 0;
    reader.for_each_entries(|entry, input| {
        assert_eq!(entry.name(), name);
        let output = path.with_extension("out");
        let bytes = copy_count(
            input,
            &mut File::options().write(true).create_new(true).open(&output)?,
            &AtomicBool::new(false),
        )?;
        assert_eq!(bytes as usize, expected.len());
        assert_eq!(fs::read(&output)?, expected);
        fs::remove_file(output)?;
        visited += 1;
        Ok(true)
    })?;
    assert_eq!(visited, 1);
    println!(
        "7z list/extract/test {}: {} bytes consumed",
        name,
        expected.len()
    );
    Ok(())
}

fn seven_large(path: &Path) -> Result<()> {
    let mut writer = ArchiveWriter::create(path)?;
    writer.push_archive_entry(
        ArchiveEntry::new_file("repeat.bin"),
        Some(io::repeat(b'A').take(1024 * 1024)),
    )?;
    writer.finish()?;
    let mut reader = ArchiveReader::open(path, Password::empty())?;
    let mut total = 0;
    reader.for_each_entries(|entry, source| {
        assert_eq!(entry.name(), "repeat.bin");
        let mut buf = [0; 8192];
        loop {
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            assert!(buf[..n].iter().all(|byte| *byte == b'A'));
            total += n;
        }
        Ok(true)
    })?;
    assert_eq!(total, 1024 * 1024);
    println!("7z 1 MiB streaming create/read: {} bytes", total);
    Ok(())
}

fn main() -> Result<()> {
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("ferrum-archive-spike-{}", std::process::id()))
        });
    fs::create_dir_all(&destination)?;
    if fs::read_dir(&destination)?.next().is_some() {
        return Err(format!(
            "fixture output directory must be empty: {}",
            destination.display()
        )
        .into());
    }
    let normal_zip = destination.join("owned.zip");
    let encrypted_zip = destination.join("encrypted.zip");
    let normal_seven = destination.join("owned.7z");
    let alias_seven = destination.join("alias.7zip");
    let large_zip = destination.join("repeat.zip");
    let slip_zip = destination.join("traversal.zip");
    let slip_seven = destination.join("traversal.7z");
    let empty_zip = destination.join("empty.zip");
    ZipWriter::new(File::create(&empty_zip)?).finish()?;
    assert!(ZipArchive::new(File::open(&empty_zip)?)?.is_empty());
    let empty_seven = destination.join("empty.7z");
    ArchiveWriter::create(&empty_seven)?.finish()?;
    assert!(
        ArchiveReader::open(&empty_seven, Password::empty())?
            .archive()
            .files
            .is_empty()
    );
    println!("ZIP/7z empty archives: listed zero entries");
    zip_create(&normal_zip, "nested/東京.txt", TEXT, false)?;
    zip_check(&normal_zip, "nested/東京.txt", TEXT, false)?;
    zip_create(&encrypted_zip, "nested/東京.txt", TEXT, true)?;
    assert!(
        ZipArchive::new(File::open(&encrypted_zip)?)?
            .by_index(0)
            .is_err()
    );
    zip_check(&encrypted_zip, "nested/東京.txt", TEXT, true)?;
    assert!(
        ZipArchive::new(File::open(&encrypted_zip)?)?
            .by_index_decrypt(0, b"wrong")
            .is_err()
    );
    println!("ZIP AES-256 password required/wrong password: detected");
    seven_create(&normal_seven, "nested/東京.txt", TEXT, false)?;
    seven_check(&normal_seven, "nested/東京.txt", TEXT, false)?;
    let encrypted_seven = destination.join("encrypted.7z");
    seven_create(&encrypted_seven, "nested/東京.txt", TEXT, true)?;
    assert!(ArchiveReader::open(&encrypted_seven, Password::empty()).is_err());
    assert!(ArchiveReader::open(&encrypted_seven, Password::from("wrong")).is_err());
    seven_check(&encrypted_seven, "nested/東京.txt", TEXT, true)?;
    println!("7z AES-256 encrypted header/password required/wrong password: detected");
    fs::copy(&normal_seven, &alias_seven)?;
    let mut signature = [0; 6];
    File::open(&alias_seven)?.read_exact(&mut signature)?;
    assert_eq!(&signature, SEVEN_SIGNATURE);
    seven_check(&alias_seven, "nested/東京.txt", TEXT, false)?;
    println!("7zip extension: signature accepted (extension alone not trusted)");
    zip_large(&large_zip)?;
    check_large(&large_zip)?;
    seven_large(&destination.join("repeat.7z"))?;
    let mut flipped = fs::read(&normal_zip)?;
    let data_offset = ZipArchive::new(File::open(&normal_zip)?)?
        .by_index(0)?
        .data_start() as usize;
    flipped[data_offset] ^= 0x80;
    let damaged_zip = destination.join("damaged-payload.zip");
    fs::write(&damaged_zip, flipped)?;
    let mut damaged = ZipArchive::new(File::open(&damaged_zip)?)?;
    assert!(io::copy(&mut damaged.by_index(0)?, &mut io::sink()).is_err());
    println!("ZIP data corruption: full stream test rejected");
    let cancel = AtomicBool::new(false);
    let mut sink = StopAfterFirstWrite {
        cancelled: &cancel,
        bytes: 0,
    };
    let mut archive = ZipArchive::new(File::open(&large_zip)?)?;
    assert_eq!(
        copy_count(&mut archive.by_index(0)?, &mut sink, &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(sink.bytes, 8192);
    println!(
        "cooperative ZIP copy cancellation after {} bytes; archive staging not implemented",
        sink.bytes
    );
    cancel.store(false, Ordering::Relaxed);
    let mut sink = StopAfterFirstWrite {
        cancelled: &cancel,
        bytes: 0,
    };
    let mut reader = ArchiveReader::open(destination.join("repeat.7z"), Password::empty())?;
    let aborted = reader.for_each_entries(|_, source| {
        copy_count(source, &mut sink, &cancel)?;
        Ok(true)
    });
    assert!(aborted.is_err());
    assert_eq!(sink.bytes, 8192);
    println!(
        "cooperative 7z copy cancellation after {} bytes; archive staging not implemented",
        sink.bytes
    );
    zip_create(&slip_zip, "../outside.txt", TEXT, false)?;
    seven_create(&slip_seven, "../outside.txt", TEXT, false)?;
    assert_eq!(
        ZipArchive::new(File::open(&slip_zip)?)?.by_index(0)?.name(),
        "../outside.txt"
    );
    assert_eq!(
        ArchiveReader::open(&slip_seven, Password::empty())?
            .archive()
            .files[0]
            .name(),
        "../outside.txt"
    );
    println!("traversal entry names accepted by readers: extraction MUST validate before writing");
    let mut broken = fs::read(&normal_seven)?;
    broken[12] ^= 0x80;
    fs::write(destination.join("corrupt.7z"), broken)?;
    assert!(ArchiveReader::open(destination.join("corrupt.7z"), Password::empty()).is_err());
    let mut damaged_seven_bytes = fs::read(&normal_seven)?;
    let normal_seven_reader = ArchiveReader::open(&normal_seven, Password::empty())?;
    let packed_offset = 32 + normal_seven_reader.archive().pack_pos() as usize;
    assert!(normal_seven_reader.archive().pack_sizes()[0] > 0);
    damaged_seven_bytes[packed_offset] ^= 0x80;
    let damaged_seven = destination.join("damaged-payload.7z");
    fs::write(&damaged_seven, damaged_seven_bytes)?;
    let mut damaged = ArchiveReader::open(&damaged_seven, Password::empty())?;
    assert!(
        damaged
            .for_each_entries(|_, entry| {
                io::copy(entry, &mut io::sink())?;
                Ok(true)
            })
            .is_err()
    );
    println!("7z data corruption: full stream test rejected");
    let mut broken = fs::read(&normal_zip)?;
    broken.truncate(broken.len() / 2);
    fs::write(destination.join("truncated.zip"), broken)?;
    assert!(ZipArchive::new(File::open(destination.join("truncated.zip"))?).is_err());
    println!("corrupt 7z start header and truncated ZIP: rejected");
    let rar_path = std::env::args_os().nth(2).map(PathBuf::from);
    if let Some(rar_path) = rar_path {
        let listing = unrar::Archive::new(&rar_path).open_for_listing()?;
        let names = listing
            .map(|item| item.map(|header| header.filename))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        println!(
            "RAR listed {} entries from user-provided legal fixture",
            names.len()
        );
        let mut cursor = unrar::Archive::new(&rar_path).open_for_processing()?;
        let mut tested = 0;
        while let Some(header) = cursor.read_header()? {
            cursor = header.test()?;
            tested += 1;
        }
        assert_eq!(tested, names.len());
        println!("RAR tested {} entry streams without extraction", tested);
    } else {
        println!(
            "RAR library linked; no legally generated RAR fixture: list/extract/test/password UNTESTED"
        );
    }
    println!("fixtures: {}", destination.display());
    Ok(())
}
