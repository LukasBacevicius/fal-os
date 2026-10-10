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

export interface CameraView { x: number; y: number; zoom: number; width: number; height: number }

export interface Stats { fps: number; frames: number; nodes: number; visible: number; detail: number; videos: number; zoom: number }

export type Tool = 'select' | 'hand';

/** A picked spot / region on a media node (Alt-click / Alt-drag), normalized 0..1. */
export interface Pick {
  nodeId: string;
  region: { kind: 'point'; x: number; y: number } | { kind: 'box'; x: number; y: number; w: number; h: number };
  /** Video playhead (seconds) for video nodes. */
  time: number | null;
}

export interface Presence {
  actor: string;
  name: string;
  kind: string;
  cursor: [number, number] | null;
  status?: string | null;
  selection?: string[];
  /** 0..1; idle cursors fade out. */
  opacity?: number;
}
