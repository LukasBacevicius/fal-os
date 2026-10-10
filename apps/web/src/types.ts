// JSON shapes produced by crates/client (see src/api.rs).

export type NodeKind =
  | 'image' | 'video' | 'audio' | 'text' | 'model3d' | 'vector' | 'document' | 'embed'
  | 'timeline' | 'stream' | 'generation' | 'frame' | 'group' | 'note' | 'connector' | string;

export interface NodeView {
  id: string;
  kind: NodeKind;
  name: string;
  x: number; y: number; w: number; h: number;
  parent: string | null;
  depth: number;
  children: string[];
  createdBy: string;
  updatedBy: string;
  text: string | null;
  props: Record<string, unknown>;
}

export interface Lineage {
  model: string;
  params: unknown;
  seed?: number;
  parents: string[];
  fal_request_id?: string;
  cost?: number;
  actor: string;
}

export interface AssetView {
  id: string;
  mime?: string;
  uri?: string;
  lineage?: Lineage;
}

export interface NodeDetail extends NodeView {
  asset: AssetView | null;
  createdByName: string | null;
  annotations: AnnotationView[];
}

export type Region =
  | { kind: 'point'; x: number; y: number }
  | { kind: 'box'; x: number; y: number; w: number; h: number }
  | { kind: 'polygon'; points: [number, number][] }
  | { kind: 'mask'; asset: string };

export type Anchor =
  | Region
  | { kind: 'time'; t: number }
  | { kind: 'range'; t0: number; t1: number }
  | { kind: 'frame'; t: number; region: Region | null }
  | { kind: 'text_range'; start: number; end: number }
  | { kind: 'page'; n: number; region: Region | null }
  | { kind: 'point3d'; p: [number, number, number] };

export type Status = 'open' | 'in_progress' | 'resolved';

export interface Message { author: string; text: string; at: number }

export interface AnnotationView {
  id: string;
  nodeId: string;
  author: string;
  status: Status;
  anchor: Anchor;
  thread: Message[];
  assignee: string | null;
  outcome: string[];
  createdAt: number;
}

export interface CameraView { x: number; y: number; zoom: number; width: number; height: number }

export interface Stats { fps: number; frames: number; nodes: number; visible: number; detail: number; videos: number; zoom: number }

export interface Draft {
  nodeId: string;
  region: Region;
  time: number | null;
  screen: [number, number] | null;
  /** Region (or point) in CSS px, for placing the composer beside it. */
  rect: [number, number, number, number] | null;
}

export type Tool = 'select' | 'hand' | 'annotate';

export interface Presence {
  actor: string;
  name: string;
  kind: string;
  color: string;
  cursor: [number, number] | null;
  status?: string | null;
  selection?: string[];
}
