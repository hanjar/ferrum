# AI chat UI (AI-07 / FER-164)

Status: UI + store built against injected (fake) executor. **Not end-to-end**: no real executor,
provider, or context resolver is wired. Nothing here runs file operations.

## Files

| File                                                           | Role                                                                                              |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `src/lib/ai/chatStore.svelte.ts`                               | `ChatStore`, `ChatExecutor` interface, `RunUpdate`/`RunView` types, `statusLabel`, `jobsProgress` |
| `src/lib/components/ai/ChatPanel.svelte`                       | Panel: log, composer, close                                                                       |
| `src/lib/components/ai/PlanCard.svelte`                        | Per-run card: status, source/target, scope, conflicts, items, jobs, result, Run/Edit/Cancel       |
| `src/lib/components/ai/MarkdownText.svelte`, `safeMarkdown.ts` | Safe text/Markdown (no `{@html}`, no links/images)                                                |
| `src/lib/ai/fixtures/chatFixtures.ts`                          | `samplePlan()`, `FakeExecutor` for tests/dev                                                      |

## Executor contract (what the real executor must implement)

`ChatExecutor`: `submit({requestId,text})`, `approve(runId)`, `cancel(runId)`, `edit(runId)`,
`subscribe(listener) => unsubscribe`, optional `listItems(runId, offset, limit)`.
The executor pushes `RunUpdate { runId, requestId, state, plan?, receipt?, jobs?, conflicts?,
blockedReason?, errorMessage?, modelText? }`. The store maps `state` to the label itself
(`statusLabel`); `modelText` is shown in a separate "Assistant note" block and never drives status.
Rules enforced by the store:

- `plan`/`receipt` are re-validated with `parseResolvedPlan`/`parseExecutionReceipt`; invalid data
  turns the run `failed` ("nothing was run").
- Updates with an unknown `runId`, mismatched `requestId`, or arriving after a terminal state are dropped
  (late provider responses after cancel).
- `approve` is accepted once and only from `awaitingApproval` (double-click safe); the real executor
  must still bind approval to plan digest/revision (AI-05).
- Typing `batal`/`cancel`/`stop` cancels the active run locally, without calling the provider.
- Cancel before queueing closes the run locally; during `queued`/`running` the card shows
  "Cancelling…" until the executor reports a terminal state.
- Large lists: conflicts are paged 20/page in the card, plan items are fetched via `listItems` 50/page,
  jobs render at most 50. Nothing renders an unbounded list.

## Accessibility / input

- `aside[aria-label="AI assistant"]`, `role="log"` conversation, `role="status"` per card, `role="alert"` for blocked/error.
- Enter sends, Shift+Enter newline, Escape calls `onclose`. All keydown events inside the panel call
  `stopPropagation`, so window-level file manager shortcuts do not fire while focus is in the chat.
- On mount the composer is focused; on destroy focus returns to the element focused at mount
  (or `returnFocus` prop) if still in the DOM.
- At `max-width: 640px` the panel becomes a full-viewport overlay. Colors use existing CSS tokens only.

## Handoff for integrator (not applied: owned by integrator)

Props: `ChatPanel({ store: ChatStore, onclose: () => void, returnFocus?: HTMLElement | null })`.

Proposed `App.svelte` (sketch):

```svelte
<script lang="ts">
  import ChatPanel from './lib/components/ai/ChatPanel.svelte';
  import { ChatStore } from './lib/ai/chatStore.svelte';
  import { aiExecutor } from './lib/ai/executor'; // lane executor, when ready
  const chat = new ChatStore(aiExecutor);
  let chatOpen = $state(false);
</script>

{#if chatOpen}<ChatPanel store={chat} onclose={() => (chatOpen = false)} />{/if}
```

Proposed action in `actions.ts`: `view.aiChat` ("Toggle AI assistant"), `run: () => toggle chatOpen`,
suggested shortcut `Ctrl+Shift+A` (verify no collision). Expose the open flag via a small store
or callback; do not store chat history in localStorage (nothing is persisted by `ChatStore`).
Layout: panel is `width: 360px; height: 100%`, place it as a right-side column in the main flex row.

## Not verified

No desktop/Tauri smoke, no real executor/provider, no visual review in the running app (jsdom tests only).
