// SPDX-License-Identifier: GPL-3.0-or-later

export {};

declare global {
  interface Window {
    flowmuxMarkdownReady: Promise<void>;
  }
}

async function render(): Promise<void> {
  const blocks = document.querySelectorAll("main.markdown-body pre > code.language-mermaid");
  if (!blocks.length) return;
  const { default: mermaid } = await import("mermaid");
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    theme: "default",
    fontFamily: getComputedStyle(document.documentElement).fontFamily,
    suppressErrorRendering: true,
  });
  await document.fonts.ready;

  for (const code of Array.from(blocks)) {
    const pre = code.parentElement!;
    const chart = document.createElement("div");
    chart.className = "mermaid-chart";
    pre.before(chart);
    try {
      const { svg } = await mermaid.render(
        `flowmux-mermaid-${crypto.randomUUID()}`,
        code.textContent ?? "",
        chart,
      );
      chart.innerHTML = svg;
      pre.remove();
    } catch (error) {
      chart.remove();
      const message = document.createElement("pre");
      message.className = "mermaid-error";
      message.setAttribute("role", "status");
      message.textContent = `Mermaid: ${error instanceof Error ? error.message : String(error)}`;
      pre.after(message);
    }
  }
}

window.flowmuxMarkdownReady = render();
