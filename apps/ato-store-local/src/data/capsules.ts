import type { Capsule } from "../types";

// Mock catalog ejected (Day 6 of Feature 2 sprint).
// The store now sources entries exclusively from the live registry at
// `/v1/manifest/capsules`. Keep this array empty so any stale fallback
// callers see "no capsules yet" instead of fake demo entries.
export const capsules: Capsule[] = [];
