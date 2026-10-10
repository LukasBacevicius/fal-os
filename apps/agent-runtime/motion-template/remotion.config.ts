import os from "node:os";
import { Config } from "@remotion/cli/config";

Config.setVideoImageFormat("jpeg");
Config.setOverwriteOutput(true);
Config.setCodec("h264");
Config.setPixelFormat("yuv420p");
// The Motion agent renders on a laptop next to a running app: leave some cores free.
Config.setConcurrency(Math.max(2, Math.floor(os.cpus().length / 2)));
