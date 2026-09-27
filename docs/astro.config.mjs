// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

// The site deploys to GitHub Pages at https://noemaforge.github.io/sethu/.
// `base` matches the repository name so assets resolve under that path.
export default defineConfig({
  site: "https://noemaforge.github.io",
  base: "/sethu",
  integrations: [
    starlight({
      title: "Sethu",
      description:
        "Trace OpenAPI changes into your application, repair them, and prove it.",
      social: {
        github: "https://github.com/noemaforge/sethu",
      },
      editLink: {
        baseUrl: "https://github.com/noemaforge/sethu/edit/main/docs",
      },
      // Four small groups rather than one long list. A reader
      // can tell from the group label whether a page gets
      // them started, shows the demo, explains the model,
      // or lists exact flags.
      sidebar: [
        {
          label: "Start here",
          items: [
            { label: "Start here", link: "/" },
            { label: "Install and first run", slug: "install" },
          ],
        },
        {
          label: "Demo",
          items: [{ label: "Immich walkthrough", slug: "walkthrough" }],
        },
        {
          label: "How it works",
          items: [
            { label: "Workflow and evidence", slug: "workflow" },
            { label: "Limits and provenance", slug: "limits" },
          ],
        },
        {
          label: "Reference",
          items: [{ label: "Command reference", slug: "commands" }],
        },
      ],
    }),
  ],
});
