import { always, eventually, now } from "@antithesishq/bombadil";
import {
  body,
  cursorOn,
  escape,
  exactRow,
  explore,
  fileStatus,
  line,
  resize,
  rows,
  text,
} from "./common.ts";
import type { State } from "@antithesishq/bombadil/terminal";

const help = (state: State, title: string) => exactRow(state, `Help: ${title}`);
const files = (state: State) =>
  rows(state).some((row) => /^ NOR  Files(?:\s|$)/.test(row)) &&
  rows(state).some((row) => /^\s*diff\.txt(?:\s|$)/.test(row.split("│")[0]));
const changes = (state: State) =>
  line(state, 0).split("│")[0].trimEnd() === "[Changes]  History";
const history = (state: State) =>
  line(state, 0).split("│")[0].trimEnd() === "Changes  [History]";
const previewMarker = (state: State, marker: string) =>
  rows(state).some((row) =>
    new RegExp(`^\\s*\\d+ [-+]${marker}_\\d{2}$`).test(row.split("│").at(-1)!),
  );
const diffFocused = (state: State) =>
  /^(?:HUN|LIN)(?:\s+\d+)?$/.test(
    line(state, state.grid.size.rows - 2)
      .split("│")
      .at(-1)!
      .trim(),
  );
const original = (state: State, content: string, selected: string) =>
  body(state, content) &&
  cursorOn(state, selected) &&
  fileStatus(state, "origin.txt [+]");

const exploration = explore([
  {
    name: "origin-ready",
    action: resize(100, 30),
    reached: (state) => body(state, "abcdef"),
  },
  {
    name: "unsaved-origin-inserted",
    action: text(`iUNSAVED_${escape}`),
    reached: (state) => original(state, "UNSAVED_abcdef", "_"),
  },
  {
    name: "unsaved-origin-selected",
    action: text("ggll"),
    reached: (state) => original(state, "UNSAVED_abcdef", "S"),
  },
  { name: "files-opened", action: text(":files\r"), reached: files },
  {
    name: "files-help-opened",
    action: text("?"),
    reached: (state) => help(state, "Files"),
  },
  {
    name: "files-help-resized",
    action: [resize(82, 24), resize(118, 38)],
    reached: (state) =>
      help(state, "Files") && [82, 118].includes(state.grid.size.columns),
  },
  {
    name: "files-help-scrolled",
    action: [text("\x04\x04\x04\x04"), text("j".repeat(48))],
    reached: (state) =>
      help(state, "Files") &&
      exactRow(state, "Global") &&
      line(state, 2) !==
        '"                    Choose the register for the next command',
  },
  {
    name: "files-help-restored",
    action: text("gg"),
    reached: (state) =>
      help(state, "Files") &&
      line(state, 2) ===
        '"                    Choose the register for the next command',
  },
  { name: "files-help-closed", action: text(escape), reached: files },
  {
    name: "changes-opened",
    action: text(":git\r"),
    reached: (state) =>
      changes(state) && previewMarker(state, "BASELINE_ALPHA"),
  },
  {
    name: "changes-preview-focused",
    action: text(":git.preview\r"),
    reached: (state) =>
      line(state, 0).split("│").at(-1)!.trim() === "Unstaged" &&
      previewMarker(state, "BASELINE_ALPHA") &&
      diffFocused(state),
  },
  {
    name: "diff-line-selection",
    action: text("a"),
    reached: (state) =>
      /^LIN(?:\s+\d+)?$/.test(
        line(state, state.grid.size.rows - 2)
          .split("│")
          .at(-1)!
          .trim(),
      ),
  },
  {
    name: "changes-preview-scrolled",
    action: text("ge"),
    reached: (state) =>
      previewMarker(state, "WORKTREE_BETA") && diffFocused(state),
  },
  {
    name: "changes-preview-resized",
    action: [resize(90, 26), resize(124, 40)],
    reached: (state) =>
      line(state, 0).split("│").at(-1)!.trim() === "Unstaged" &&
      previewMarker(state, "WORKTREE_BETA") &&
      [90, 124].includes(state.grid.size.columns),
  },
  {
    name: "diff-help-opened",
    action: text(":help\r"),
    reached: (state) => help(state, "Diff"),
  },
  {
    name: "diff-help-closed",
    action: text(escape),
    reached: (state) =>
      line(state, 0).split("│").at(-1)!.trim() === "Unstaged" &&
      previewMarker(state, "WORKTREE_BETA"),
  },
  {
    name: "first-return-preserves-selection",
    action: text(":q\r"),
    reached: (state) => original(state, "UNSAVED_abcdef", "S"),
  },
  {
    name: "first-return-edited",
    action: text("r@"),
    reached: (state) => original(state, "UN@AVED_abcdef", "@"),
  },
  { name: "changes-reopened", action: text(":git\r"), reached: changes },
  {
    name: "history-opened",
    action: text(":git.page-history\r"),
    reached: (state) =>
      history(state) &&
      rows(state).some((row) =>
        /^[a-f0-9]{8} Terminal fixture baseline\s*│/.test(row),
      ),
  },
  {
    name: "commit-files-opened",
    action: text("\r"),
    reached: (state) =>
      /^Changes  \[History\]  Commit › [a-f0-9]{8}$/.test(
        line(state, 0).split("│")[0].trimEnd(),
      ) &&
      /^diff\.txt\s+A\s+\+40\s+-0$/.test(
        line(state, 1).split("│")[0].trimEnd(),
      ),
  },
  {
    name: "commit-preview-focused",
    action: text(":git-commit-files.preview\r"),
    reached: (state) =>
      previewMarker(state, "BASELINE_ALPHA") && diffFocused(state),
  },
  {
    name: "commit-preview-resized",
    action: [resize(86, 25), resize(120, 36)],
    reached: (state) =>
      previewMarker(state, "BASELINE_ALPHA") &&
      [86, 120].includes(state.grid.size.columns),
  },
  {
    name: "files-from-commit-preview",
    action: text(":files\r"),
    reached: files,
  },
  {
    name: "second-return-preserves-selection",
    action: text(":q\r"),
    reached: (state) => original(state, "UN@AVED_abcdef", "@"),
  },
  {
    name: "second-return-edited",
    action: text("r#"),
    reached: (state) => original(state, "UN#AVED_abcdef", "#"),
  },
  {
    name: "origin-saved",
    action: text(":w\r"),
    reached: (state) =>
      body(state, "UN#AVED_abcdef") && fileStatus(state, "origin.txt"),
  },
  {
    name: "closed-normally",
    action: text(":q\r"),
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
