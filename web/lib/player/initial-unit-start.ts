/** Bind a timestamp to the entry unit, not to the number of effect executions. */
export function createInitialUnitStart(initialUnitKey: string) {
  let leftInitialUnit = false;
  return (unitKey: string, startMsOverride: number | undefined): number | undefined => {
    if (unitKey !== initialUnitKey) leftInitialUnit = true;
    return leftInitialUnit ? undefined : startMsOverride;
  };
}
