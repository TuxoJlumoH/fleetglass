import { initialLayoutState, type Tab } from "../stores/layout";

/** A Pods resources tab, the initial tab before 02-08b made it "Overview". */
export const PODS_TAB: Tab = {
  id: "pods",
  title: "Pods",
  view: "resources",
  kind: "Pod",
  cluster: null,
  namespace: null,
  textFilter: "",
};

/** Layout state starting on a Pods tab, for tests of the resource table and its chrome. */
export const podsLayoutState = {
  ...initialLayoutState,
  tabs: [PODS_TAB],
  activeTabId: PODS_TAB.id,
};
