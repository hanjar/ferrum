/**
 * Branching on archive failures by their structured code (FER-73), never by message text.
 * Kept apart from `archive.ts` (which talks to Tauri) so it stays pure and trivially testable.
 */
import { FsError } from './types';

/** True when the error means the archive needs a password, or the one given was wrong. Uses the
 *  structured code, never the message text. */
export function isPasswordError(e: unknown): boolean {
  return (
    e instanceof FsError &&
    (e.archiveCode === 'passwordRequired' || e.archiveCode === 'badPassword')
  );
}

export function isCancelledError(e: unknown): boolean {
  return e instanceof FsError && (e.code === 'cancelled' || e.archiveCode === 'cancelled');
}

export function isMultipartError(e: unknown): boolean {
  return (
    e instanceof FsError &&
    (e.archiveCode === 'multipartNotFirst' || e.archiveCode === 'multipartMissing')
  );
}
