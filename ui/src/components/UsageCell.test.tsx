import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { UsageCell } from "./UsageCell";

describe("UsageCell", () => {
  it("renders a muted dash when usage is empty, null, or unmeasured", () => {
    const { container: c1 } = render(<UsageCell cell={null} />);
    expect(c1.querySelector(".use-empty")).toHaveTextContent("—");

    const { container: c2 } = render(<UsageCell cell={undefined} />);
    expect(c2.querySelector(".use-empty")).toHaveTextContent("—");

    const { container: c3 } = render(
      <UsageCell cell={{ value: 0, formatted: "", request: null, limit: null }} />,
    );
    expect(c3.querySelector(".use-empty")).toHaveTextContent("—");

    const { container: c4 } = render(
      <UsageCell cell={{ value: 0, formatted: "—", request: null, limit: null }} />,
    );
    expect(c4.querySelector(".use-empty")).toHaveTextContent("—");
  });

  it("renders limit-relative usage with normal color when < 80%", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 40,
          formatted: "40m",
          request: 20,
          limit: 100,
        }}
      />,
    );

    expect(screen.getByText("40m")).toBeInTheDocument();
    expect(screen.getByText("40% lim")).toBeInTheDocument();

    const reqBar = container.querySelector(".bar i.req");
    expect(reqBar).not.toBeNull();
    expect(reqBar).toHaveStyle({ width: "20%" });

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).not.toBeNull();
    expect(mainBar).toHaveStyle({ width: "40%" });
    expect(mainBar).not.toHaveClass("warn");
    expect(mainBar).not.toHaveClass("err");
  });

  it("renders limit-relative usage with .warn when >= 80% and <= 100%", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 118,
          formatted: "118Mi",
          request: 100,
          limit: 128,
        }}
      />,
    );

    // 118 / 128 = 92.1875% -> 92% lim
    expect(screen.getByText("118Mi")).toBeInTheDocument();
    expect(screen.getByText("92% lim")).toBeInTheDocument();

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).toHaveClass("warn");
    expect(mainBar).not.toHaveClass("err");
    expect(mainBar).toHaveStyle({ width: "92%" });
  });

  it("renders limit-relative usage with .err and clamped width when > 100%", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 150,
          formatted: "150m",
          request: 50,
          limit: 100,
        }}
      />,
    );

    // 150 / 100 = 150% -> 150% lim, clamped bar 100%
    expect(screen.getByText("150m")).toBeInTheDocument();
    expect(screen.getByText("150% lim")).toBeInTheDocument();

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).toHaveClass("err");
    expect(mainBar).toHaveStyle({ width: "100%" });
  });

  it("renders request-relative usage when limit is not set", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 3,
          formatted: "3m",
          request: 100,
          limit: null,
        }}
      />,
    );

    expect(screen.getByText("3m")).toBeInTheDocument();
    expect(screen.getByText("3% req")).toBeInTheDocument();

    const reqBar = container.querySelector(".bar i.req");
    expect(reqBar).not.toBeNull();
    expect(reqBar).toHaveStyle({ width: "100%" });

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).toHaveStyle({ width: "3%" });
    expect(mainBar).not.toHaveClass("warn");
    expect(mainBar).not.toHaveClass("err");
  });

  it("renders empty subtext and 0% bar when neither limit nor request is set", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 0,
          formatted: "0m",
          request: null,
          limit: null,
        }}
      />,
    );

    expect(screen.getByText("0m")).toBeInTheDocument();
    const subtext = container.querySelector(".v span");
    expect(subtext).toHaveTextContent("");

    const reqBar = container.querySelector(".bar i.req");
    expect(reqBar).toBeNull();

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).toHaveStyle({ width: "0%" });
  });

  it("renders node usage where limit is allocatable and request is null", () => {
    const { container } = render(
      <UsageCell
        cell={{
          value: 4000,
          formatted: "4",
          request: null,
          limit: 8000,
        }}
      />,
    );

    expect(screen.getByText("4")).toBeInTheDocument();
    expect(screen.getByText("50% lim")).toBeInTheDocument();

    const reqBar = container.querySelector(".bar i.req");
    expect(reqBar).toBeNull();

    const mainBar = container.querySelector(".bar i:not(.req)");
    expect(mainBar).toHaveStyle({ width: "50%" });
  });
});
