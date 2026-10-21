<script lang="ts">
  /**
   * Review & approve (AP-3, AP-4, AP-5, AP-6). The test result and the approval outcome are shown
   * exactly as the backend reports them. "Approve" is enabled only after a passed test; clicking
   * it asks the backend, which shows the native confirmation. The client keeps no "approved"
   * state: `canceled` leaves the user here, `stale` shows one fixed text and asks for a new test.
   */
  import { reviewLabel } from '../../ipc/automation';
  import Mnemonic from '../Mnemonic.svelte';
  import type { AutomationStore, ReviewState } from '../../stores/automationStore.svelte';
  import {
    RUNTIME_BACKGROUND_DESC,
    dstNote,
    missedText,
    nextRunLines,
    scheduleSummary,
    watchPolicyLines,
    watchSummary,
  } from './automationModel';

  let {
    store,
    review,
    oneditprofile,
  }: {
    store: AutomationStore;
    review: ReviewState;
    oneditprofile?: (id: string) => void;
  } = $props();

  const draft = $derived(review.draft);
  const profile = $derived(store.profile(draft.profileId));
  const trigger = $derived(draft.policy.trigger);
  const test = $derived(review.test);
  const compareStep = $derived(test?.steps.find((s) => s.id === 'compare'));
  const opsMatch = $derived(compareStep?.text.match(/(\d+)\s+operations?/));
  const failedStep = $derived(test?.steps.find((s) => s.status === 'fail'));
  const nextLines = $derived(
    trigger.kind === 'schedule' && store.editor
      ? nextRunLines(store.editor.next, trigger.tz, Date.now())
      : [],
  );

  const approveLabel = $derived(
    review.mode === 'new'
      ? 'Approve & enable'
      : review.mode === 'edit'
        ? 'Approve & save'
        : 'Approve & resume',
  );
  const stepGlyph = { ok: '✓', warn: '!', fail: '✕' } as const;
  const stepLabel = { ok: 'OK', warn: 'Warning', fail: 'Failed' } as const;
</script>

<div class="review" data-testid="automation-review">
  <section>
    <h3>What will run</h3>
    {#if profile}
      <div class="line">
        <strong>{profile.name}</strong> · rev {profile.revision}
      </div>
      <div class="line paths">Source / target: {profile.left} ⇄ {profile.right}</div>
      <div class="line">
        {profile.modeLabel}{profile.deleteLabel ? ` · ${profile.deleteLabel}` : ''}
      </div>
    {:else}
      <div class="line">
        <strong>{review.row?.profileName ?? 'Sync profile'}</strong>
        {#if review.row}· rev {review.row.profileRevision}{/if}
      </div>
    {/if}
    <button type="button" class="link" onclick={() => oneditprofile?.(draft.profileId)}>
      Edit sync profile ↗
    </button>
  </section>

  {#if review.row?.review}
    <section data-testid="review-reason">
      <h3>Why a review is needed</h3>
      <div class="line">{reviewLabel(review.row.review)}</div>
    </section>
  {/if}

  {#if review.changes.length > 0}
    <section data-testid="profile-changes">
      <h3>Profile changes since the approved revision</h3>
      <ul class="changes">
        {#each review.changes as c, i (i)}
          <li><span class="field">{c.field}</span>: {c.from} → {c.to}</li>
        {/each}
      </ul>
    </section>
  {/if}

  <section>
    <h3>Effect on your files</h3>
    {#if compareStep}
      <div class="line">{compareStep.text}</div>
    {:else}
      <div class="line muted">Run the test to see the effect on your files.</div>
    {/if}
  </section>

  <section>
    <h3>Trigger</h3>
    <div class="line">
      {review.mode === 'resume' && review.row
        ? review.row.trigger
        : trigger.kind === 'schedule'
          ? scheduleSummary(trigger)
          : watchSummary(trigger)}
    </div>
    {#if nextLines.length > 0}
      <ol class="next">
        {#each nextLines as l, i (i)}
          <li>
            {l.label} <span class="muted">{l.relative}</span>
            {#if l.equivalent}<span class="muted">{l.equivalent}</span>{/if}
          </li>
        {/each}
      </ol>
    {/if}
  </section>

  <section>
    <h3>Runtime</h3>
    {#if draft.policy.runtime === 'background'}
      <div class="line">In the background</div>
      <div class="line muted">{RUNTIME_BACKGROUND_DESC}</div>
    {:else}
      <div class="line">While Ferrum is running</div>
      <div class="line muted">
        Enabled doesn't mean always running: this automation runs only while a Ferrum window is
        open.
      </div>
    {/if}
  </section>

  <section>
    <h3>Policies</h3>
    <ul class="policies">
      {#if trigger.kind === 'schedule'}
        <li>{missedText(trigger.missed)}</li>
        {#if dstNote(trigger.tz)}<li>{dstNote(trigger.tz)}</li>{/if}
      {:else}
        {#each watchPolicyLines(trigger) as line, i (i)}
          <li data-testid="watch-policy">{line}</li>
        {/each}
      {/if}
      <li>Overlapping runs: a run waits while another uses the same folders.</li>
      <li>Conflicts: held for review, never overwritten.</li>
      <li>Errors: transient errors are retried up to {draft.policy.retries} times.</li>
      <li>Offline location: the run waits until the folder is available.</li>
      <li>Stopping a run: it stops after the current file; finished files are not rolled back.</li>
    </ul>
  </section>

  <section class="test" aria-label="Test run">
    <h3>Test run</h3>
    {#if review.testing}
      <div class="line muted" role="status">Testing…</div>
    {/if}
    {#if test}
      <ol class="steps" data-testid="test-steps">
        {#each test.steps as s, i (s.id)}
          <li class={s.status} style="animation-delay: {i * 70}ms" data-step={s.id}>
            <span class="glyph" role="img" aria-label={stepLabel[s.status]}
              >{stepGlyph[s.status]}</span
            >
            <span>{s.text}</span>
          </li>
        {/each}
      </ol>
      {#if test.passed}
        <div class="passed" role="status" data-testid="test-passed">
          Test passed — no files were changed.{opsMatch
            ? ` The comparison found ${opsMatch[1]} operations; the real run recompares first.`
            : ' The real run recompares first.'}
        </div>
      {:else if failedStep}
        <div class="failed" role="alert" data-testid="test-failed">{failedStep.text}</div>
      {/if}
    {/if}
    <div>
      <button
        type="button"
        disabled={review.testing || review.approving}
        onclick={() => void store.runTest()}
      >
        <Mnemonic label={test ? 'Run &test again' : 'Run &test'} />
      </button>
    </div>
  </section>

  {#if review.notice}
    <p
      class="banner {review.notice.kind}"
      role={review.notice.kind === 'error' ? 'alert' : 'status'}
      data-testid="review-notice"
    >
      {review.notice.text}
    </p>
  {/if}

  {#if store.resumeBlock}
    <p class="banner error" role="alert" data-testid="resume-block">{store.resumeBlock}</p>
  {/if}
  {#if review.stale && review.mode === 'edit'}
    <p class="banner">
      <button type="button" onclick={() => void store.reloadEdit()}>Reload</button>
      Reload opens the editor again with the latest saved version.
    </p>
  {/if}

  <footer class="footer">
    <button type="button" disabled={review.approving} onclick={() => store.backFromReview()}>
      Back
    </button>
    <button
      type="button"
      class="primary"
      disabled={!store.canApprove}
      title={store.canApprove ? undefined : 'Run a test that passes first'}
      onclick={() => void store.approve()}
    >
      <Mnemonic label={'Appr&ove ' + approveLabel.slice('Approve '.length).replace('&', '&&')} />
    </button>
  </footer>
</div>

<style>
  .review {
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow: auto;
    min-height: 0;
    padding-right: 4px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  h3 {
    margin: 0;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text2);
  }
  .line {
    font-size: 12.5px;
  }
  .paths {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text2);
  }
  .muted {
    color: var(--text3);
    font-size: 11.5px;
  }
  .changes,
  .policies,
  .next {
    margin: 0;
    padding-left: 18px;
    font-size: 12px;
  }
  .policies {
    color: var(--text2);
  }
  .field {
    font-weight: 600;
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .steps li {
    display: flex;
    gap: 6px;
    align-items: baseline;
    animation: stepIn 160ms ease-out both;
  }
  @keyframes stepIn {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
  }
  .glyph {
    width: 14px;
    text-align: center;
    font-weight: 700;
  }
  .ok .glyph {
    color: var(--greenInk);
  }
  .warn .glyph {
    color: var(--amberInk);
  }
  .fail .glyph {
    color: var(--redInk);
  }
  .passed {
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--greenSoft);
    color: var(--greenInk);
    font-size: 12px;
  }
  .failed {
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--redSoft);
    color: var(--redInk);
    font-size: 12px;
  }
  .banner {
    margin: 0;
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
    color: var(--text2);
  }
  .banner.error {
    border: 1px solid var(--red);
  }
  .link {
    align-self: flex-start;
    height: 22px;
    padding: 0 6px;
    border: 0;
    background: transparent;
    color: var(--accentInk);
    font-size: 12px;
    text-decoration: underline;
    cursor: default;
  }
  .footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    position: sticky;
    bottom: 0;
    padding-top: 8px;
    background: var(--elev);
  }
  .test button,
  .footer button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  button:disabled {
    opacity: 0.45;
  }
  .footer button.primary {
    background: var(--accent);
    color: #fff;
  }
</style>
