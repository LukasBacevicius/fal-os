import type { SVGProps } from 'react';

type P = SVGProps<SVGSVGElement> & { size?: number };

function Svg({ size = 16, children, ...rest }: P) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8}
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" {...rest}>
      {children}
    </svg>
  );
}

export const IconSelect = (p: P) => <Svg {...p}><path d="M5 3l14 7-6 2-2 6z" /></Svg>;
export const IconHand = (p: P) => (
  <Svg {...p}><path d="M8 13V5.5a1.5 1.5 0 013 0V12M11 11.5v-7a1.5 1.5 0 013 0V12M14 12V6.5a1.5 1.5 0 013 0V15a6 6 0 01-6 6h-1a6 6 0 01-5-2.7L3.3 15a1.6 1.6 0 012.5-2L8 15" /></Svg>
);
export const IconComment = (p: P) => (
  <Svg {...p}><path d="M4 20l1.6-4A8 8 0 1112 20a8.3 8.3 0 01-3.2-.6z" /><path d="M12 9v6M9 12h6" /></Svg>
);
export const IconUpload = (p: P) => <Svg {...p}><path d="M12 16V4M7 9l5-5 5 5M4 16v3a1 1 0 001 1h14a1 1 0 001-1v-3" /></Svg>;
export const IconImage = (p: P) => <Svg {...p}><rect x="3" y="4" width="18" height="16" rx="2" /><circle cx="9" cy="10" r="2" /><path d="M21 16l-5-5-9 9" /></Svg>;
export const IconVideo = (p: P) => <Svg {...p}><rect x="3" y="5" width="13" height="14" rx="2" /><path d="M16 10l5-3v10l-5-3" /></Svg>;
export const IconAudio = (p: P) => <Svg {...p}><path d="M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2" /></Svg>;
export const IconCube = (p: P) => <Svg {...p}><path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z" /><path d="M4 7.5l8 4.5 8-4.5M12 12v9" /></Svg>;
export const IconDoc = (p: P) => <Svg {...p}><path d="M6 3h8l4 4v14H6z" /><path d="M14 3v4h4M9 12h6M9 16h6" /></Svg>;
export const IconText = (p: P) => <Svg {...p}><path d="M5 6V4h14v2M12 4v16M9 20h6" /></Svg>;
export const IconFrame = (p: P) => <Svg {...p}><path d="M7 3v18M17 3v18M3 7h18M3 17h18" /></Svg>;
export const IconNote = (p: P) => <Svg {...p}><path d="M5 4h14v10l-6 6H5z" /><path d="M13 20v-6h6" /></Svg>;
export const IconSparkle = (p: P) => <Svg {...p}><path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z" /><path d="M19 17l.7 1.8L21.5 19.5l-1.8.7L19 22l-.7-1.8-1.8-.7 1.8-.7z" /></Svg>;
export const IconLayers = (p: P) => <Svg {...p}><path d="M12 3l9 5-9 5-9-5z" /><path d="M3 13l9 5 9-5" /></Svg>;
export const IconCheck = (p: P) => <Svg {...p}><path d="M5 12l5 5 9-10" /></Svg>;
export const IconX = (p: P) => <Svg {...p}><path d="M6 6l12 12M18 6L6 18" /></Svg>;
export const IconMore = (p: P) => <Svg {...p}><circle cx="5" cy="12" r="1" /><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /></Svg>;
export const IconUndo = (p: P) => <Svg {...p}><path d="M9 14L4 9l5-5" /><path d="M4 9h10a6 6 0 010 12h-3" /></Svg>;
export const IconRedo = (p: P) => <Svg {...p}><path d="M15 14l5-5-5-5" /><path d="M20 9H10a6 6 0 000 12h3" /></Svg>;
export const IconSend = (p: P) => <Svg {...p}><path d="M5 12h14M13 6l6 6-6 6" /></Svg>;
export const IconTarget = (p: P) => <Svg {...p}><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="3" /></Svg>;
export const IconTrash = (p: P) => <Svg {...p}><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13" /></Svg>;
export const IconChevron = (p: P) => <Svg {...p}><path d="M9 6l6 6-6 6" /></Svg>;
export const IconFit = (p: P) => <Svg {...p}><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" /></Svg>;
export const IconBox = (p: P) => <Svg {...p}><rect x="4" y="4" width="16" height="16" rx="2" strokeDasharray="3 3" /></Svg>;
export const IconPin = (p: P) => <Svg {...p}><path d="M12 21s7-6.2 7-12a7 7 0 10-14 0c0 5.8 7 12 7 12z" /><circle cx="12" cy="9" r="2.5" /></Svg>;
export const IconClock = (p: P) => <Svg {...p}><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></Svg>;
export const IconLink = (p: P) => <Svg {...p}><path d="M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1" /><path d="M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1-1" /></Svg>;

export function KindIcon({ kind, size = 14 }: { kind: string; size?: number }) {
  const p = { size };
  switch (kind) {
    case 'image': return <IconImage {...p} />;
    case 'video': return <IconVideo {...p} />;
    case 'audio': return <IconAudio {...p} />;
    case 'model3d': return <IconCube {...p} />;
    case 'document': return <IconDoc {...p} />;
    case 'text': return <IconText {...p} />;
    case 'frame': return <IconFrame {...p} />;
    case 'note': return <IconNote {...p} />;
    case 'generation': return <IconSparkle {...p} />;
    case 'embed': return <IconLink {...p} />;
    default: return <IconBox {...p} />;
  }
}
