# Research: Game Market & AI in Art Production (July 2026)

Compiled 2026-07-17 from web sources; for the new-alert project (retro RTS,
RA-era 2D sprite art). Source links at bottom.

## 1. Market context

- Global games revenue: **$188.8B in 2025 (+3.4% YoY), projected ~$205B in
  2026** (Newzoo). ~3.6B players. Mobile ≈52% of revenue; console the
  fastest-growing platform (~5.5%). Mature single-digit growth, not
  pandemic-era expansion.
- **Layoff wave has crested but not ended**: ~45,000 jobs lost 2022→mid-2025
  (8.5k in 2022, 10.5k in 2023, 14.6k peak in 2024, ~4k through July 2025).
  Embracer alone closed/divested 44 studios. Practical effect: lots of
  experienced dev/art talent freelancing, and publishers risk-averse toward
  big-budget bets — a tailwind for small, focused, nostalgia-driven projects.
- **RTS is in a genuine (modest) revival**: 2025 called "the strongest year
  for RTS in ages" but still no breakout mainstream hit. Tempest Rising
  (explicitly C&C-styled, April 2025) hit Steam's top-12 sellers with 89%
  positive; Stormgate struggled. Remasters/retro do well: AoM Retold,
  Stronghold Crusader DE (90% positive). Takeaway: the audience for exactly
  our aesthetic exists and is underserved; the bar for "feels right" is set by
  C&C Remastered.

## 2. AI in the art pipeline — what's actually used

- Adoption is real but shallower than hype: **~36% of game professionals use
  genAI at work** (GDC 2026 survey), but only **19% for asset generation** —
  the dominant uses are research/brainstorming (81%) and daily-task assistance
  (47%). Studio devs use it less (30%) than publishing/marketing staff (58%).
- Where it works in production (consistent across studio write-ups):
  **artist-led pipelines** — AI for exploration, variation, and base-pass
  generation; humans own final quality via paintover as a standard stage.
  Common stack: Midjourney/Flux for concept exploration; **Stable Diffusion /
  ComfyUI locally with custom LoRAs and style anchors** for on-model batch
  assets; texture base-pass → Substance/Photoshop refinement. Claimed ~40%
  time reduction on specific workflows (vendor-adjacent number; treat as
  upper bound).
- **Pixel/sprite art specifically**: the standout tool is **Retro Diffusion**
  (Astropulse) — models that *enforce* limited palettes and true low-res
  output, 16×16–512×512, sprite-sheet generation (walk cycles, idle, VFX
  loops), available as a $65 local Aseprite extension. Scenario hosts it too.
  Pixel-art LoRAs on SD/Flux are the DIY alternative.
- **Precedent that matters for us — C&C Remastered (2020)**: Lemon Sky
  **hand-redrew every sprite** at high resolution (no AI upscaling for game
  art); AI upscaling was used only for FMV cutscenes, from PlayStation-quality
  sources, because the masters were lost — and those cutscenes are the most
  criticized part of the remaster. Lesson: for sprite art, regeneration beats
  upscaling; naive AI upscale of 24px sprites reads as mush.

## 3. Labor / legal / platform

- **Sentiment is sharply negative and worsening**: 52% of game professionals
  say genAI harms the industry (30% in 2025, 18% in 2024); worst among visual
  artists (64%). Publicly leaning on AI art carries community-backlash risk,
  especially in retro/indie circles.
- **Steam requires AI disclosure** (since Jan 2024; rules rewritten Jan 2026):
  must disclose AI-generated content that **ships in the game or marketing**;
  behind-the-scenes tools (Copilot, ideation concept art) explicitly exempt.
  30.8% of 2026 Steam releases carry an AI disclosure (10.9% in 2024);
  disclosure itself doesn't hurt sales much (68.6% of surveyed players fine
  with or indifferent to it), but "AI slop" flooding (60–90% of release
  growth) makes quality signaling matter more.
- **Legal status still unsettled**: first final judgment (Thomson Reuters v.
  Ross, Feb 2025) went against the AI side on fair use; Bartz v. Anthropic
  settled for $1.5B over pirated training copies; Getty v. Stability and the
  artist class action (Stability/Midjourney/DeviantArt) still active, big
  trials expected 2026-27. Outputs remain uncopyrightable without human
  authorship (SCOTUS declined to revisit, Mar 2026) — pure-AI assets can't be
  protected, another reason for human-in-the-loop workflows.

## 4. Practical takeaways for new-alert

1. **Phase 1 needs no art decisions**: we render the original freeware assets.
   No AI, no disclosure question, byte-faithful look. This is the project's
   moat — real assets, modern engine. (EA's freeware release covers the
   original games' assets; our GPL engine is clean-room-adjacent with the GPL
   source as reference. Distributing *assets* stays out of our repo either way.)
2. **If/when we need new art** (UI chrome, new units, marketing, or a
   "remastered" art option): the realistic 2026 workflow is Retro Diffusion or
   a pixel-art LoRA in ComfyUI, constrained to the RA 256-color palette,
   with hand cleanup in Aseprite — generation, not upscaling. Budget for
   human paintover; pure generations are neither shippable-quality nor
   copyrightable.
3. **Disclose on Steam if AI-touched content ships**; don't ship AI slop —
   the market punishes it via discoverability collapse, not the disclosure.
4. **Commercial positioning**: retro RTS demand is proven (Tempest Rising) and
   remaster appetite is real; faithful reproduction + modern QoL (resolution,
   netcode, replays) is exactly the product shape that's working.

## Sources

- https://www.pcgamer.com/gaming-industry/the-videogame-market-is-as-big-as-ever-with-pc-leading-growth-global-games-revenue-surpassed-the-usd200-billion-mark-in-2025/
- https://instreamly.com/posts/video-game-industry-revenue-in-2026-3-6-billion-players-and-a-188-billion-market/
- https://en.wikipedia.org/wiki/2022%E2%80%932026_video_game_industry_layoffs
- https://www.gamedeveloper.com/business/industry-layoffs-are-seemingly-slowing-but-the-damage-has-already-been-done
- https://gdconf.com/article/gdc-2026-state-of-the-game-industry-reveals-impact-of-layoffs-generative-ai-and-more/
- https://www.gamedeveloper.com/business/one-third-of-game-workers-use-generative-ai-but-half-think-it-s-bad-for-the-industry
- https://80.lv/articles/gdc-survey-over-50-of-game-devs-say-generative-ai-harms-industry
- https://inkration.com/ai-art-tools-for-game-studios-in-2026-what-actually-works-for-production/
- https://www.strayspark.studio/blog/comfyui-game-asset-pipeline-indie-2026
- https://www.retrodiffusion.ai/
- https://astropulse.itch.io/retrodiffusion
- https://fragwyz.substack.com/p/three-years-of-ai-on-steam
- https://www.gamesradar.com/games/steam-study-of-over-53-000-games-finds-60-90-percent-of-the-growth-in-monthly-releases-on-valves-store-is-from-games-using-ai-and-almost-none-of-them-make-money/
- https://www.pcgamer.com/software/ai/steam-updates-ai-disclosure-form-to-specify-that-its-focused-on-ai-generated-content-that-is-consumed-by-players-not-efficiency-tools-used-behind-the-scenes/
- https://www.xda-developers.com/dislike-ai-content-in-your-games-68-of-steam-users-disagree-with-you-says-survey/
- https://www.gamepressure.com/newsroom/2025-is-the-strongest-year-for-rts-in-ages-but-the-big-breakout-h/zc8400
- https://www.gamesmarket.global/rts-comeback-tempest-rising-c1c6cc29485464f744cc0f2658ec1fc5/
- https://www.nortonrosefulbright.com/en/knowledge/publications/ce8eaa5f/ai-in-litigation-series-an-update-on-ai-copyright-cases-in-2026
- https://copyrightalliance.org/ai-copyright-lawsuit-developments-2025/
- https://en.wikipedia.org/wiki/Command_&_Conquer_Remastered_Collection
- https://www.lemonskystudios.com/news/experiencing-command-conquer-remastered-with-malaysias-lemon-sky-studios/
