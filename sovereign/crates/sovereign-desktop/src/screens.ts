// SPDX-License-Identifier: AGPL-3.0-or-later
// Dev-only screen gallery entry — a no-backend "storybook" for the
// onboarding screens. Renders the real components with fixtures so you can
// audit copy, click through the flow, and check the recommended models
// WITHOUT a daemon, models, or any setup side effects. Served by
// `npm run dev` (or `npm run screens`) at /screens.html — pure browser.
import "@fontsource-variable/ibm-plex-sans";
import "@fontsource-variable/source-serif-4/opsz.css";
import "@fontsource-variable/source-serif-4/opsz-italic.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import { mount } from "svelte";
import ScreenGallery from "./lib/setup/ScreenGallery.svelte";
import "./app.css";
// The bundled model manifest — the SAME file the Rust setup planner reads
// (sovereign/models.toml, via include_str!). Imported raw so the gallery
// shows the real recommendations with zero drift.
import modelsToml from "../../../models.toml?raw";

mount(ScreenGallery, {
  target: document.getElementById("app")!,
  props: { modelsToml },
});
