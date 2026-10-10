// Prints fal's live unit prices for endpoints (platform pricing API, FAL_KEY auth).
// Usage: tsx scripts/fal-pricing.ts fal-ai/flux/dev fal-ai/stable-audio-25/text-to-audio
import { falKey } from "../src/config.ts";

const ids = process.argv.slice(2);
const qs = ids.map((i) => `endpoint_id=${encodeURIComponent(i)}`).join("&");
const res = await fetch(`https://api.fal.ai/v1/models/pricing?${qs}`, { headers: { authorization: `Key ${falKey()}` } });
console.log(res.status, (await res.text()).slice(0, 1500));
