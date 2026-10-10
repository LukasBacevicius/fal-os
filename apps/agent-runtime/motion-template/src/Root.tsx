import React from "react";
import { Composition } from "remotion";
import { Ad } from "./Ad";
import { AdProps, FPS, defaultProps } from "./props";

export const Root: React.FC = () => (
  <Composition
    id="Ad"
    component={Ad}
    width={1920}
    height={1080}
    fps={FPS}
    durationInFrames={defaultProps.durationInSeconds * FPS}
    defaultProps={defaultProps}
    calculateMetadata={({ props }: { props: AdProps }) => ({
      durationInFrames: Math.round(Math.min(60, Math.max(6, props.durationInSeconds || 15)) * FPS),
    })}
  />
);
