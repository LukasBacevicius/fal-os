import React from "react";
import {
  AbsoluteFill,
  Audio,
  Img,
  OffthreadVideo,
  Sequence,
  interpolate,
  spring,
  staticFile,
  useCurrentFrame,
  useVideoConfig,
  Easing,
} from "remotion";
import { AdProps } from "./props";

const FONT =
  '"SF Pro Display", "Helvetica Neue", Inter, "Segoe UI", system-ui, -apple-system, sans-serif';
const FADE = 10;

/** Fade a scene in and out over FADE frames. */
const SceneFade: React.FC<{ duration: number; children: React.ReactNode }> = ({ duration, children }) => {
  const f = useCurrentFrame();
  const opacity = interpolate(f, [0, FADE, duration - FADE, duration], [0, 1, 1, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  return <AbsoluteFill style={{ opacity }}>{children}</AbsoluteFill>;
};

const Backdrop: React.FC<{ p: AdProps }> = ({ p }) => {
  const f = useCurrentFrame();
  const x = 50 + Math.sin(f / 90) * 18;
  const y = 45 + Math.cos(f / 120) * 12;
  return (
    <AbsoluteFill
      style={{
        background: `radial-gradient(circle at ${x}% ${y}%, ${p.palette.accent}55 0%, ${p.palette.bg} 55%), ${p.palette.bg}`,
      }}
    />
  );
};

/** Words rise out of a mask, staggered. */
const KineticText: React.FC<{ text: string; size: number; delay?: number; color: string; weight?: number }> = ({
  text,
  size,
  delay = 0,
  color,
  weight = 700,
}) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  return (
    <div style={{ display: "flex", flexWrap: "wrap", gap: size * 0.28, lineHeight: 1.05 }}>
      {text.split(" ").map((w, i) => {
        const s = spring({ frame: f - delay - i * 4, fps, config: { damping: 200, mass: 0.6 } });
        return (
          <span key={i} style={{ overflow: "hidden", display: "inline-block", paddingBottom: size * 0.08 }}>
            <span
              style={{
                display: "inline-block",
                transform: `translateY(${(1 - s) * 110}%)`,
                fontSize: size,
                fontWeight: weight,
                letterSpacing: -size * 0.03,
                color,
                fontFamily: FONT,
              }}
            >
              {w}
            </span>
          </span>
        );
      })}
    </div>
  );
};

const KenBurns: React.FC<{ src: string; duration: number; radius?: number; from?: number; to?: number }> = ({
  src,
  duration,
  radius = 0,
  from = 1.12,
  to = 1.0,
}) => {
  const f = useCurrentFrame();
  const scale = interpolate(f, [0, duration], [from, to], { extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ overflow: "hidden", borderRadius: radius }}>
      <Img src={staticFile(src)} style={{ width: "100%", height: "100%", objectFit: "cover", transform: `scale(${scale})` }} />
    </AbsoluteFill>
  );
};

const Hero: React.FC<{ p: AdProps; duration: number }> = ({ p, duration }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: f, fps, config: { damping: 18, mass: 1.2 } });
  const sweep = interpolate(f, [10, duration * 0.7], [-60, 160], { extrapolateRight: "clamp", easing: Easing.inOut(Easing.cubic) });
  const tagline = interpolate(f, [duration * 0.45, duration * 0.65], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center" }}>
      <div
        style={{
          position: "relative",
          width: 1100,
          height: 700,
          transform: `scale(${interpolate(s, [0, 1], [1.25, 1])})`,
          opacity: s,
          borderRadius: 36,
          overflow: "hidden",
          boxShadow: `0 60px 140px ${p.palette.accent}44`,
        }}
      >
        <KenBurns src={p.hero} duration={duration} from={1.08} to={1.0} />
        <AbsoluteFill
          style={{
            background: `linear-gradient(105deg, transparent ${sweep - 20}%, rgba(255,255,255,0.35) ${sweep}%, transparent ${sweep + 20}%)`,
            mixBlendMode: "screen",
          }}
        />
      </div>
      <div style={{ position: "absolute", top: 70, width: "100%", display: "flex", justifyContent: "center" }}>
        <div style={{ letterSpacing: 18, fontSize: 34, fontWeight: 600, color: p.palette.fg, fontFamily: FONT, opacity: s }}>
          {p.brand.toUpperCase()}
        </div>
      </div>
      <div
        style={{
          position: "absolute",
          bottom: 80,
          fontFamily: FONT,
          fontSize: 56,
          fontWeight: 600,
          color: p.palette.fg,
          opacity: tagline,
          transform: `translateY(${(1 - tagline) * 20}px)`,
        }}
      >
        {p.tagline}
      </div>
    </AbsoluteFill>
  );
};

const Feature: React.FC<{ p: AdProps; i: number; duration: number; image: string }> = ({ p, i, duration, image }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const card = spring({ frame: f - 4, fps, config: { damping: 200 } });
  const detail = interpolate(f, [FADE + 12, FADE + 24], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const bar = interpolate(f, [0, duration], [0, 1], { extrapolateRight: "clamp" });
  // Text waits for the previous feature's text to leave, and leaves upward itself.
  const textIn = interpolate(f, [0, FADE], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const textOut = interpolate(f, [duration - FADE * 1.5, duration - FADE * 0.5], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const ft = p.features[i];
  return (
    <AbsoluteFill style={{ flexDirection: "row", alignItems: "center", padding: "0 120px", gap: 90 }}>
      <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 28, opacity: textIn * (1 - textOut), transform: `translateY(${-textOut * 60}px)` }}>
        <div style={{ fontFamily: FONT, fontSize: 30, fontWeight: 700, color: p.palette.accent, letterSpacing: 4 }}>
          {String(i + 1).padStart(2, "0")} / {String(p.features.length).padStart(2, "0")}
        </div>
        <KineticText text={ft.title} size={104} color={p.palette.fg} delay={FADE} />
        {ft.detail ? (
          <div style={{ fontFamily: FONT, fontSize: 42, color: p.palette.fg, opacity: detail * 0.7 }}>{ft.detail}</div>
        ) : null}
        <div style={{ height: 6, width: 420, background: `${p.palette.fg}22`, borderRadius: 3, overflow: "hidden" }}>
          <div style={{ height: "100%", width: `${bar * 100}%`, background: p.palette.accent }} />
        </div>
      </div>
      <div
        style={{
          position: "relative",
          width: 760,
          height: 760,
          transform: `translateX(${(1 - card) * 120}px) rotate(${(1 - card) * 4}deg)`,
          opacity: card,
          borderRadius: 40,
          overflow: "hidden",
          boxShadow: "0 40px 120px rgba(0,0,0,0.5)",
        }}
      >
        <KenBurns src={image} duration={duration} />
      </div>
    </AbsoluteFill>
  );
};

const Montage: React.FC<{ p: AdProps; duration: number }> = ({ p, duration }) => {
  const clips = p.clips ?? [];
  const stills = [...(p.stills ?? []), p.hero];
  const items: { kind: "clip" | "still"; src: string }[] = clips.length
    ? clips.map((src) => ({ kind: "clip" as const, src }))
    : stills.map((src) => ({ kind: "still" as const, src }));
  const per = Math.floor(duration / items.length);
  return (
    <AbsoluteFill style={{ backgroundColor: "black" }}>
      {items.map((it, i) => (
        <Sequence key={i} from={i * per} durationInFrames={i === items.length - 1 ? duration - i * per : per + FADE}>
          <SceneFade duration={i === items.length - 1 ? duration - i * per : per + FADE}>
            {it.kind === "clip" ? (
              <OffthreadVideo src={staticFile(it.src)} muted style={{ width: "100%", height: "100%", objectFit: "cover" }} />
            ) : (
              <KenBurns src={it.src} duration={per + FADE} from={1.0} to={1.15} />
            )}
          </SceneFade>
        </Sequence>
      ))}
      <AbsoluteFill style={{ background: "linear-gradient(0deg, rgba(0,0,0,0.55) 0%, transparent 35%)" }} />
      <div style={{ position: "absolute", left: 80, bottom: 70, fontFamily: FONT, fontSize: 40, fontWeight: 700, color: "white", letterSpacing: 10 }}>
        {p.brand.toUpperCase()}
      </div>
    </AbsoluteFill>
  );
};

const EndCard: React.FC<{ p: AdProps; duration: number }> = ({ p, duration }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: f, fps, config: { damping: 200 } });
  const cta = spring({ frame: f - 18, fps, config: { damping: 14 } });
  const out = interpolate(f, [duration - 12, duration], [1, 0], { extrapolateLeft: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", opacity: out }}>
      <div style={{ display: "flex", alignItems: "center", gap: 80 }}>
        <div
          style={{
            position: "relative",
            width: 620,
            height: 620,
            borderRadius: 48,
            overflow: "hidden",
            transform: `scale(${interpolate(s, [0, 1], [0.85, 1])})`,
            opacity: s,
            boxShadow: `0 50px 120px ${p.palette.accent}55`,
          }}
        >
          <KenBurns src={p.hero} duration={duration} from={1.0} to={1.06} />
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 26, maxWidth: 820 }}>
          <KineticText text={p.product} size={120} color={p.palette.fg} />
          <div style={{ fontFamily: FONT, fontSize: 48, color: p.palette.fg, opacity: s * 0.75 }}>{p.tagline}</div>
          <div
            style={{
              alignSelf: "flex-start",
              marginTop: 20,
              padding: "22px 44px",
              borderRadius: 999,
              background: p.palette.accent,
              color: "white",
              fontFamily: FONT,
              fontSize: 38,
              fontWeight: 700,
              transform: `scale(${cta})`,
            }}
          >
            {p.cta}
          </div>
        </div>
      </div>
      <div style={{ position: "absolute", bottom: 60, fontFamily: FONT, fontSize: 30, letterSpacing: 14, color: p.palette.fg, opacity: 0.6 }}>
        {p.brand.toUpperCase()}
      </div>
    </AbsoluteFill>
  );
};

export const Ad: React.FC<AdProps> = (p) => {
  const { durationInFrames, fps } = useVideoConfig();
  const f = useCurrentFrame();
  const d = durationInFrames;
  const hero = Math.round(d * 0.2);
  const features = Math.round(d * 0.34);
  const montage = Math.round(d * 0.26);
  const end = d - hero - features - montage;
  const perFeature = Math.floor(features / Math.max(1, p.features.length));
  const pool = [...(p.stills ?? []), p.hero];
  const volume = interpolate(f, [0, fps, d - fps * 1.5, d], [0, 0.9, 0.9, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  return (
    <AbsoluteFill style={{ backgroundColor: p.palette.bg }}>
      <Backdrop p={p} />
      {p.music ? <Audio src={staticFile(p.music)} volume={volume} /> : null}
      <Sequence durationInFrames={hero + FADE}>
        <SceneFade duration={hero + FADE}>
          <Hero p={p} duration={hero + FADE} />
        </SceneFade>
      </Sequence>
      {p.features.map((ft, i) => (
        <Sequence key={i} from={hero + i * perFeature} durationInFrames={perFeature + FADE}>
          <SceneFade duration={perFeature + FADE}>
            <Feature p={p} i={i} duration={perFeature + FADE} image={ft.image ?? pool[i % pool.length]} />
          </SceneFade>
        </Sequence>
      ))}
      <Sequence from={hero + features} durationInFrames={montage + FADE}>
        <SceneFade duration={montage + FADE}>
          <Montage p={p} duration={montage + FADE} />
        </SceneFade>
      </Sequence>
      <Sequence from={hero + features + montage} durationInFrames={end}>
        <EndCard p={p} duration={end} />
      </Sequence>
    </AbsoluteFill>
  );
};
