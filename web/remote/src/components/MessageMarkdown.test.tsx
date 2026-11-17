import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MessageMarkdown } from "./MessageMarkdown";

const TABLE = `| Fix | Status |
|---|---|
| Shell logs | done |
| Networks | **done** |`;

describe("MessageMarkdown", () => {
  it("renders GitHub tables as tables, scrollable on narrow screens", () => {
    const { container } = render(<MessageMarkdown text={TABLE} />);
    const table = screen.getByRole("table");
    expect(screen.getAllByRole("columnheader").map((h) => h.textContent)).toEqual(["Fix", "Status"]);
    expect(screen.getAllByRole("row")).toHaveLength(3);
    expect(table.parentElement).toHaveClass("overflow-x-auto");
    expect(container.querySelector("strong")?.textContent).toBe("done");
  });

  it("keeps the rest of GFM: strikethrough and task lists", () => {
    const { container } = render(<MessageMarkdown text={"~~old~~\n\n- [x] shipped\n- [ ] next"} />);
    expect(container.querySelector("del")?.textContent).toBe("old");
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(2);
  });

  it("still drops images and raw HTML", () => {
    const { container } = render(<MessageMarkdown text={'![x](http://e/x.png) <b onclick="x">hi</b>'} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("b")).toBeNull();
  });
});
