import type { Tree } from "@antithesishq/bombadil";
import { actions, extract, type State } from "@antithesishq/bombadil/terminal";

type StepAction =
  | { TypeText: { CharSet: [{ Literal: string }] } }
  | { Resize: { columns: number; rows: number } };

export const escape = "\x1b";
export const text = (value: string): StepAction => ({
  TypeText: { CharSet: [{ Literal: value }] },
});
export const resize = (columns: number, rows: number): StepAction => ({
  Resize: { columns, rows },
});
export const rows = (state: State): string[] =>
  Array.from({ length: state.grid.size.rows }, (_, row) =>
    state.grid.rowText(row).trimEnd(),
  );
export const line = (state: State, row: number): string =>
  state.grid.rowText(row).trimEnd();
export const exactRow = (state: State, expected: string): boolean =>
  rows(state).some((row) => row.trim() === expected);
export const body = (state: State, expected: string): boolean =>
  line(state, 0) === ` 1 ${expected}`.trimEnd();
export const cursorOn = (state: State, expected: string): boolean =>
  state.cursor.visible &&
  state.grid.row(state.cursor.position.row)[state.cursor.position.column]
    ?.contents === expected;
export const prompt = (state: State, expected: string): boolean =>
  line(state, state.grid.size.rows - 1) === expected;
export const fileStatus = (state: State, expected: string): boolean =>
  line(state, state.grid.size.rows - 2).match(
    /^ NOR  (.*?) {2,}1 sel  1:\d+  UTF-8$/,
  )?.[1] === expected;

export type Step = {
  name: string;
  action: StepAction | StepAction[];
  reached: (state: State) => boolean;
};

function matches(state: State, template: StepAction): boolean {
  if ("TypeText" in template) {
    return (
      state.lastAction !== null &&
      "TypeText" in state.lastAction &&
      state.lastAction.TypeText === template.TypeText.CharSet[0].Literal
    );
  }
  if ("Resize" in template) {
    return (
      state.lastAction !== null &&
      "Resize" in state.lastAction &&
      state.grid.size.columns === template.Resize.columns &&
      state.grid.size.rows === template.Resize.rows
    );
  }
  return false;
}

export function explore(steps: Step[]) {
  let index = 0;
  let inFlight = false;
  const checkpoints: string[] = [];
  const journey = extract((state) => {
    const step = steps[index];
    if (step !== undefined) {
      const alternatives = Array.isArray(step.action)
        ? step.action
        : [step.action];
      if (alternatives.some((action) => matches(state, action)))
        inFlight = true;
      if (inFlight && step.reached(state)) {
        checkpoints.push(step.name);
        index += 1;
        inFlight = false;
      }
    }
    return {
      step: steps[index]?.name ?? "completed",
      completed: index === steps.length,
      checkpoints: [...checkpoints],
      inFlight,
      columns: state.grid.size.columns,
      rows: state.grid.size.rows,
      screen: rows(state),
      exited: state.exitStatus !== null,
      exitCode: state.exitStatus?.code ?? null,
    };
  }).named("journey");
  const drive = actions(() => {
    const snapshot = journey.current;
    if (snapshot === null || snapshot.completed || snapshot.exited) return [];
    const step = steps.find((candidate) => candidate.name === snapshot.step)!;
    const pulse = resize(snapshot.columns, snapshot.rows);
    if (snapshot.inFlight) return [pulse];
    const alternatives = Array.isArray(step.action)
      ? step.action
      : [step.action];
    const branches: [number, Tree<StepAction>][] = alternatives.map(
      (action) => [1, { value: action }],
    );
    // Replays may finish rendering before the original run's harmless wait pulses.
    // A zero-weight leaf accepts those pulses without generating extra waits.
    branches.push([0, { value: pulse }]);
    return { branches };
  });
  return { journey, drive };
}
