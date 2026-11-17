import { Attributes, type State } from "@antithesishq/bombadil/terminal";
import { always, eventually, now } from "@antithesishq/bombadil";
import {
  body,
  cursorOn,
  escape,
  exactRow,
  explore,
  fileStatus,
  line,
  prompt,
  resize,
  text,
} from "./common.ts";

function selectedHelpCandidate(state: State) {
  const cells = state.grid.row(state.grid.size.rows - 2);
  return (
    exactRow(state, "help") &&
    exactRow(state, "Show buffer keys") &&
    cells.slice(0, 4).every(({ style }) => {
      const background = style.backgroundColor;
      return (
        typeof background === "object" &&
        "Palette" in background &&
        background.Palette === 8
      );
    }) &&
    cells.slice(4, 6).every(({ style }) => {
      const background = style.backgroundColor;
      return (
        typeof background !== "object" ||
        !("Palette" in background) ||
        background.Palette !== 8
      );
    }) &&
    [0, 2, 3].every((column) =>
      Attributes.has(cells[column].style, Attributes.Bold),
    ) &&
    !Attributes.has(cells[1].style, Attributes.Bold)
  );
}

const exploration = explore([
  {
    name: "origin-ready",
    action: resize(100, 30),
    reached: (state) => body(state, "abcdef"),
  },
  {
    name: "goto-prefix-visible",
    action: text("g"),
    reached: (state) => exactRow(state, "g"),
  },
  {
    name: "goto-prefix-cancelled",
    action: text(escape),
    reached: (state) =>
      body(state, "abcdef") && cursorOn(state, "a") && !exactRow(state, "g"),
  },
  {
    name: "insert-after-prefix-cancel",
    action: text(`iZ${escape}`),
    reached: (state) => body(state, "Zabcdef") && cursorOn(state, "Z"),
  },
  {
    name: "window-prefix-with-count",
    action: text("gg2\x17"),
    reached: (state) =>
      exactRow(state, "C-w") &&
      line(state, state.grid.size.rows - 1).trim() ===
        "2 · C-w … · Esc to cancel",
  },
  {
    name: "window-prefix-cancelled",
    action: text(escape),
    reached: (state) => body(state, "Zabcdef") && !exactRow(state, "C-w"),
  },
  {
    name: "count-does-not-leak",
    action: text("lrX"),
    reached: (state) => body(state, "ZXbcdef") && cursorOn(state, "X"),
  },
  {
    name: "first-candidate-and-documentation",
    action: text(":hlp"),
    reached: (state) => prompt(state, ":hlp") && selectedHelpCandidate(state),
  },
  {
    name: "tab-completes-command",
    action: text("\t"),
    reached: (state) =>
      prompt(state, ":help") && exactRow(state, "Show buffer keys"),
  },
  {
    name: "completed-command-cancelled",
    action: text(escape),
    reached: (state) => body(state, "ZXbcdef") && !prompt(state, ":help"),
  },
  {
    name: "enter-executes-selected-candidate",
    action: text(":hlp\r"),
    reached: (state) => exactRow(state, "Help: Text"),
  },
  {
    name: "help-returns-to-editor",
    action: text(escape),
    reached: (state) => body(state, "ZXbcdef") && cursorOn(state, "X"),
  },
  {
    name: "register-selected-for-command",
    action: text('gg"ayl"a:'),
    reached: (state) => prompt(state, ":"),
  },
  {
    name: "register-command-cancelled",
    action: text(escape),
    reached: (state) => body(state, "ZXbcdef") && !prompt(state, ":"),
  },
  {
    name: "cancel-followup-applied",
    action: text("yh"),
    reached: (state) => body(state, "ZXbcdef") && cursorOn(state, "Z"),
  },
  {
    name: "origin-saved",
    action: text(":w\r"),
    reached: (state) =>
      body(state, "ZXbcdef") && fileStatus(state, "origin.txt"),
  },
  {
    name: "new-buffer-opened",
    action: text(":new\r"),
    reached: (state) => body(state, ""),
  },
  {
    name: "named-register-preserved",
    action: text('"ap'),
    reached: (state) => body(state, "Z"),
  },
  {
    name: "new-buffer-edited",
    action: text(`AAFTER${escape}`),
    reached: (state) => body(state, "ZAFTER"),
  },
  {
    name: "register-output-saved",
    action: text(":w register-output.txt\r"),
    reached: (state) =>
      body(state, "ZAFTER") && fileStatus(state, "register-output.txt"),
  },
  {
    name: "closing-saved-buffer-returns-to-origin",
    action: text(":quit\r"),
    reached: (state) =>
      body(state, "ZXbcdef") && fileStatus(state, "origin.txt"),
  },
  {
    name: "closing-origin-returns-to-scratch",
    action: text(":q\r"),
    reached: (state) =>
      body(state, "") && fileStatus(state, "[scratch]"),
  },
  {
    name: "closed-normally",
    action: text(":quit\r"),
    reached: (state) => state.exitStatus?.code === 0,
  },
]);

const journey = exploration.journey;
export const drive = exploration.drive;
export const completesJourney = eventually(
  () => journey.current.completed,
).within(30, "seconds");
export const exitsOnlyAfterSaving = always(
  now(() => !journey.current.exited || journey.current.completed),
);
