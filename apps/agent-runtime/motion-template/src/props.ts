/**
 * Props of the `Ad` composition. All media are file names inside `public/` (copy canvas
 * assets there first); they're loaded with `staticFile()`.
 */
export type AdProps = {
  brand: string;
  product: string;
  /** Short line under the hero, e.g. "Silence, redesigned." */
  tagline: string;
  /** 2–4 feature callouts shown as kinetic type. `image` is optional (file in public/). */
  features: { title: string; detail?: string; image?: string }[];
  /** Hero product still (public/ file). */
  hero: string;
  /** Extra stills for the montage when there are few clips. */
  stills?: string[];
  /** Video clips for the montage (public/ files). */
  clips?: string[];
  /** Soundtrack (public/ file). */
  music?: string;
  /** End card call to action, e.g. "Available now · brand.com". */
  cta: string;
  palette: { bg: string; fg: string; accent: string };
  /** Total length; scenes scale to it. */
  durationInSeconds: number;
};

export const defaultProps: AdProps = {
  brand: "AURA",
  product: "Aura One",
  tagline: "Silence, redesigned.",
  features: [
    { title: "Adaptive noise cancelling", detail: "Tunes itself 200× a second" },
    { title: "40-hour battery", detail: "A week of commutes" },
    { title: "Spatial audio", detail: "Sound that surrounds you" },
  ],
  hero: "hero.png",
  stills: [],
  clips: [],
  cta: "Available now",
  palette: { bg: "#0b0b0f", fg: "#f5f5f7", accent: "#7c5cff" },
  durationInSeconds: 15,
};

export const FPS = 30;
