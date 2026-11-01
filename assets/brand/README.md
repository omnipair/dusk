# Omnipair light visual defaults

Use [omnipair-light.json](omnipair-light.json) for Omnipair documents, Notion visuals, charts, diagrams, presentations and UI mockups. Light is the user's default unless a task asks for another theme. This does not change the theme of any deployed application.

Colors were verified against [brand.omnipair.fi](https://brand.omnipair.fi/) on 31 October 2026. The [shades and neutrals page](https://brand.omnipair.fi/?view=topics&topic=shades) includes UI tokens and warm neutrals missing from the downloaded color document. UI orange `80` is `#F89831`; the Dawn gradient uses `#F29C5B`. Keep that distinction.

The configuration contains official palette values and a separate local mapping from tokens to background, surface, text and chart roles. The default treatment is warm monochrome: neutral 50 background, white or neutral 100 surfaces, neutral 900 ink and neutral 200 borders. Use spacing, typography and labels to distinguish content. A single small brand accent is optional; multicolor cards and gradients are not the default.

`msaansvf.woff2` is the existing Omnipair M Saans font copied from `dusk-webapp/public/fonts/`. The proposal renderer embeds it so the uploaded mockup does not depend on another website loading a font.

Notion's native paragraphs and tables cannot use arbitrary hex colors. Keep them native and readable; apply the exact brand palette to uploaded figures and interactive HTML. Preserve page mentions and unrelated edits when replacing visual attachments.

The risk-pricing proposal renderer reads this JSON. Senior and junior use neutral tones and explicit labels, without a blue/orange risk color code.

Use `omnipair-lockup.svg` for the logo. It is the supplied symbol and wordmark asset from the Omnipair webapp brand directory, consistent with the official horizontal lockup. Preserve its aspect ratio and clear space. Never type “omnipair” in a font as a substitute logo. The downloaded `omnipair-horizontal-black-light.png` is the official brand presentation reference.
