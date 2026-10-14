export interface Pos { path: number[]; off: number }

export interface Fmt {
  font: string;
  theme: string | null;
  size: number | null;
  b: boolean;
  i: boolean;
  u: boolean;
  s: boolean;
  valign: string | null;
  color: string | null;
  highlight: string | null;
  style: string | null;
  jc: string | null;
  line: number | null;
  lineRule: string | null;
  list: { id: number; bullet: boolean } | null;
  indLeft: number;
  indRight: number;
  indFirst: number;
  styleName: string | null;
}

export interface State {
  anchor: Pos;
  focus: Pos;
  collapsed: boolean;
  caret: { page: number; x: number; y: number; h: number } | null;
  sel: [number, number, number, number, number][];
  /** edits are recorded as tracked changes */
  track: boolean;
  undo: boolean;
  redo: boolean;
  version: number;
  /** changes of the document itself (not of its layout) */
  edits: number;
  table: boolean;
  /** a selected picture: its box in points */
  image: { page: number; x: number; y: number; w: number; h: number; path: number[]; off: number } | null;
  change: { kind: string; author: string; date: string } | null;
  changes: number;
  markup: boolean;
  comment: string | null;
  comments: number;
  /** header or footer being edited */
  /** header, footer or footnote being edited; a footnote also tells where it is on its page */
  story: { kind: string; page: number; top?: number; bottom?: number } | null;
  fmt: Fmt;
}

export interface Face { family: string; bold: boolean; italic: boolean; file: string }

export type Item = [string, ...any[]];
export interface DisplayList { w: number; h: number; items: Item[] }

export interface CommentItem {
  id: string;
  author: string;
  initials: string;
  date: string;
  text: string;
  done: boolean;
  paraId: string | null;
  parent: string | null;
  anchored: boolean;
}
