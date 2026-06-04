import { mount } from "svelte";
import App from "./App.svelte";
// Lavender Court fonts — same fontsource bundle the desktop ships.
import "@fontsource-variable/ibm-plex-sans";
import "@fontsource-variable/source-serif-4/opsz.css";
import "@fontsource-variable/source-serif-4/opsz-italic.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./app.css";

const app = mount(App, { target: document.getElementById("app")! });

export default app;
