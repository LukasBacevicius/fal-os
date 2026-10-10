import { useRef } from 'react';
import { useStore } from '../store';
import { importFiles } from '../controller';
import { IconComment, IconHand, IconSelect, IconUpload } from '../icons';
import type { Tool } from '../types';

const TOOLS: { id: Tool; label: string; key: string; Icon: typeof IconSelect }[] = [
  { id: 'select', label: 'Move', key: 'V', Icon: IconSelect },
  { id: 'hand', label: 'Hand', key: 'H', Icon: IconHand },
  { id: 'annotate', label: 'Annotate', key: 'C', Icon: IconComment },
];

export function Toolbar() {
  const app = useStore((s) => s.app);
  const tool = useStore((s) => s.tool);
  const file = useRef<HTMLInputElement>(null);
  return (
    <div className="toolbar" role="toolbar" aria-label="Tools">
      {TOOLS.map(({ id, label, key, Icon }) => (
        <button
          key={id}
          className={`tool ${tool === id ? 'active' : ''}`}
          title={`${label} (${key})`}
          aria-label={label}
          aria-pressed={tool === id}
          onClick={() => app?.setTool(id)}
        >
          <Icon size={18} />
        </button>
      ))}
      <div className="toolbar-sep" />
      <button className="tool" title="Import files" aria-label="Import files" onClick={() => file.current?.click()}>
        <IconUpload size={18} />
      </button>
      <input
        ref={file}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          importFiles(Array.from(e.target.files ?? []));
          e.target.value = '';
        }}
      />
    </div>
  );
}
