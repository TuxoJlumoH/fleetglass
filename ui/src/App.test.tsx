import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { App } from "./App";
import { useAppInfoStore } from "./stores/appInfo";
import { initialLayoutState, useLayoutStore } from "./stores/layout";

describe("App shell", () => {
  beforeEach(() => {
    useLayoutStore.setState(initialLayoutState);
    useAppInfoStore.setState({ version: null });
  });

  it("renders sidebar empty state, tabs and status bar", () => {
    render(<App />);
    expect(screen.getByRole("navigation", { name: "Contexts" })).toBeInTheDocument();
    expect(screen.getByText("No contexts loaded")).toBeInTheDocument();
    expect(screen.getByRole("tablist", { name: "Open views" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
    // Outside the desktop app the overview says so instead of showing empty cards.
    expect(screen.getByText("Not running in the Fleetglass app")).toBeInTheDocument();
    expect(screen.getByRole("contentinfo")).toBeInTheDocument();
    expect(screen.getByTestId("app-version")).toHaveTextContent("version: —");
  });

  it("shows the version once it is known", () => {
    useAppInfoStore.setState({ version: "0.1.0" });
    render(<App />);
    expect(screen.getByTestId("app-version")).toHaveTextContent("v0.1.0");
  });

  it("toggles the sidebar with Ctrl+B", async () => {
    const user = userEvent.setup();
    render(<App />);
    expect(screen.getByRole("navigation", { name: "Contexts" })).toBeVisible();
    await user.keyboard("{Control>}b{/Control}");
    expect(screen.queryByRole("navigation", { name: "Contexts" })).toBeNull();
    await user.keyboard("{Control>}b{/Control}");
    expect(screen.getByRole("navigation", { name: "Contexts" })).toBeVisible();
  });
});

describe("layout store", () => {
  beforeEach(() => {
    useLayoutStore.setState({
      ...initialLayoutState,
      tabs: [
        { id: "a", title: "A" },
        { id: "b", title: "B" },
      ],
      activeTabId: "a",
    });
  });

  it("cycles tabs with wrap-around", () => {
    const { cycleTab } = useLayoutStore.getState();
    cycleTab(1);
    expect(useLayoutStore.getState().activeTabId).toBe("b");
    cycleTab(1);
    expect(useLayoutStore.getState().activeTabId).toBe("a");
    cycleTab(-1);
    expect(useLayoutStore.getState().activeTabId).toBe("b");
  });

  it("ignores unknown tab ids", () => {
    useLayoutStore.getState().setActiveTab("nope");
    expect(useLayoutStore.getState().activeTabId).toBe("a");
  });
});
