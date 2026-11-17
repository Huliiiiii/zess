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
const commitFiles = (state: State) =>
  /^Changes  \[History\]  Commit › [a-f0-9]{8}$/.test(
    line(state, 0).split("│")[0].trimEnd(),
  ) &&
  /^diff\.txt\s+A\s+\+40\s+-0$/.test(
    line(state, 1).split("│")[0].trimEnd(),
  );
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
const historicalSearches = new Map(
  [5, 17, 33].map((index) => {
    const marker = `BASELINE_ALPHA_${String(index).padStart(2, "0")}`;
    return [text(`/${marker}\r`), { number: index + 1, marker }] as const;
  }),
);
let historicalSourceRow: string | undefined;

function mousePosition(screen: string[], marker: string): string | undefined {
  for (const [row, text] of screen.entries()) {
    const column = text.indexOf(marker);
    if (column >= 0) return `${column + 1};${row + 1}`;
  }
}

function doubleClickFile(screen: string[]) {
  const position = mousePosition(screen, "diff.txt");
  if (position === undefined) return [];
  return [
    text(`\x1b[<0;${position}M\x1b[<0;${position}m`.repeat(2)),
  ];
}

function dragAdditions(screen: string[]) {
  const start = mousePosition(screen, "+WORKTREE_BETA_35");
  const end = mousePosition(screen, "+WORKTREE_BETA_36");
  if (start === undefined || end === undefined) return [];
  return [text(`\x1b[<0;${start}M\x1b[<32;${end}M\x1b[<0;${end}m`)];
}

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
    name: "double-click-moves-from-the-clicked-file",
    action: doubleClickFile,
    reached: (state) =>
      files(state) &&
      rows(state).some((row) => row.split("│").at(-1)?.trim() === "abcdef"),
  },
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
    reached: (state) => help(state, "Git diff"),
  },
  {
    name: "diff-help-closed",
    action: text(escape),
    reached: (state) =>
      line(state, 0).split("│").at(-1)!.trim() === "Unstaged" &&
      previewMarker(state, "WORKTREE_BETA"),
  },
  {
    name: "closing-changes-returns-to-files",
    action: text(":quit\r"),
    reached: files,
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
    reached: commitFiles,
  },
  {
    name: "active-history-tab-keeps-commit-files",
    action: text("\x172"),
    reached: commitFiles,
  },
  {
    name: "changes-from-commit-files",
    action: text(":git-commit-files.page-changes\r"),
    reached: changes,
  },
  {
    name: "history-restores-commit-files",
    action: text(":git.page-history\r"),
    reached: commitFiles,
  },
  {
    name: "active-history-command-keeps-commit-files",
    action: text(":git-commit-files.page-history\r"),
    reached: commitFiles,
  },
  {
    name: "commit-preview-focused",
    action: text(":git-commit-files.preview\r"),
    reached: (state) =>
      previewMarker(state, "BASELINE_ALPHA") && diffFocused(state),
  },
  {
    name: "historical-source-line-selected",
    action: [...historicalSearches.keys()],
    reached: (state, action) => {
      const selected = historicalSearches.get(action);
      const status = line(state, state.grid.size.rows - 2).split("│").at(-1);
      const selectedLine = status?.trim().match(/^(?:HUN|LIN)\s+(\d+)$/)?.[1];
      if (selected === undefined || selectedLine !== String(selected.number)) {
        return false;
      }
      historicalSourceRow = `${selected.number} ${selected.marker}`;
      return true;
    },
  },
  {
    name: "historical-source-opens-at-the-selected-line",
    action: text("e"),
    reached: (state) =>
      state.cursor.visible &&
      line(state, state.cursor.position.row).trim() === historicalSourceRow,
  },
  {
    name: "closing-historical-source-restores-preview-focus",
    action: text(":q\r"),
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
    name: "closing-files-returns-to-commit-files",
    action: text(":quit\r"),
    reached: commitFiles,
  },
  {
    name: "closing-commit-files-returns-to-history",
    action: text(":q\r"),
    reached: history,
  },
  {
    name: "second-return-preserves-selection",
    action: text(":quit\r"),
    reached: (state) => original(state, "UN@AVED_abcdef", "@"),
  },
  {
    name: "second-return-edited",
    action: text("r#"),
    reached: (state) => original(state, "UN#AVED_abcdef", "#"),
  },
  {
    name: "changes-reopened-for-staging",
    action: text(":git\r"),
    reached: (state) =>
      changes(state) &&
      (previewMarker(state, "BASELINE_ALPHA") ||
        previewMarker(state, "WORKTREE_BETA")),
  },
  {
    name: "staging-preview-focused",
    action: text(":git.preview\r"),
    reached: (state) => diffFocused(state),
  },
  {
    name: "unstaged-additions-visible",
    action: text("ge"),
    reached: (state) => dragAdditions(rows(state)).length > 0,
  },
  {
    name: "mouse-selected-additions",
    action: dragAdditions,
    reached: (state) =>
      /^RNG\s+37$/.test(
        line(state, state.grid.size.rows - 2).split("│").at(-1)!.trim(),
      ),
  },
  {
    name: "selected-additions-staged",
    action: text(" s"),
    reached: (state) => exactRow(state, "Git index updated"),
  },
  {
    name: "closing-changes-restores-the-edited-file",
    action: text(":quit\r"),
    reached: (state) => original(state, "UN#AVED_abcdef", "#"),
  },
  {
    name: "origin-saved",
    action: text(":w\r"),
    reached: (state) =>
      body(state, "UN#AVED_abcdef") && fileStatus(state, "origin.txt"),
  },
  {
    name: "closing-origin-returns-to-scratch",
    action: text(":quit\r"),
    reached: (state) =>
      body(state, "") && fileStatus(state, "[scratch]"),
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
