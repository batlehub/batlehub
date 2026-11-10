import type { ResolutionState } from "@/components/ui/resolution";

/**
 * A hold's verdict state as DESIGN.md's Resolution draws it: denied is
 * blocked (crimson, all four cells), quarantined is held (copper, three).
 * The console lists holds only, so `warned` and `allowed` have no entry.
 */
export const HOLD_RESOLUTION: Record<string, ResolutionState> = {
  denied: "blocked",
  quarantined: "held",
};

/** The word beside the matrix, spelled out so the catalogue test sees the keys. */
export const HOLD_LABEL_KEYS: Record<string, string> = {
  denied: "holdsPanel.denied",
  quarantined: "holdsPanel.quarantined",
};
