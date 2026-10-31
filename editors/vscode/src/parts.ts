// `Engines: Add a Part…` (ADR 0039) — the part that doesn't need VS Code: the picked action must still
// match the text it's applied to. Pure (tested without VS Code).

/**
 * The action to apply for `picked`, which engines-lsp computed against version `seen` of the doc. If
 * the doc changed while the list was open (Claude's write reloading a clean buffer, say), its edit
 * is for the old text — and `workspace.applyEdit` doesn't check versions — so ask again
 * (`refetch`) and take the action with the same title. `undefined` when the part no longer fits as
 * it did, or the doc won't hold still.
 */
export async function currentAction<A extends { title: string }>(
  picked: A,
  seen: number,
  version: () => number,
  refetch: () => Promise<A[]>,
  tries = 3,
): Promise<A | undefined> {
  let action = picked;
  while (version() !== seen) {
    if (tries-- <= 0) {
      return undefined;
    }
    seen = version();
    const again = (await refetch()).find((a) => a.title === picked.title);
    if (!again) {
      return undefined;
    }
    action = again;
  }
  return action;
}
