import "@fontsource/syne/400.css";
import "@fontsource/syne/500.css";
import "@fontsource/syne/600.css";
import "@fontsource/syne/700.css";
import "@fontsource/syne/800.css";
import "@fontsource/syne-mono/400.css";
import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
