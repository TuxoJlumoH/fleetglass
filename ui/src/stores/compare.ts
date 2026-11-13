import { create } from "zustand";

export interface CompareTarget {
  cluster: string;
  kind: string;
  namespace?: string | null | undefined;
  name: string;
  uid?: string | undefined;
  manifest?: string | undefined;
}

export interface CompareState {
  selectedForCompare: CompareTarget | null;
  selectForCompare: (target: CompareTarget) => void;
  clearCompare: () => void;
  activeComparison: {
    left: CompareTarget;
    right: CompareTarget;
  } | null;
  openComparison: (left: CompareTarget, right: CompareTarget) => void;
  closeComparison: () => void;
}

export const useCompareStore = create<CompareState>((set) => ({
  selectedForCompare: null,
  selectForCompare: (target) => {
    set({ selectedForCompare: target });
  },
  clearCompare: () => {
    set({ selectedForCompare: null });
  },
  activeComparison: null,
  openComparison: (left, right) => {
    set({ activeComparison: { left, right } });
  },
  closeComparison: () => {
    set({ activeComparison: null });
  },
}));
