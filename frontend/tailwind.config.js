/** Tokens, not a theme: every value here answers a question this console asks. */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // A deep, slightly blue black — a terminal that has been looked at
        // for hours, not a marketing hero.
        base: "#0B0E14",
        raised: "#131822",
        sunk: "#080A0F",
        line: "#1E2635",
        line2: "#2A3648",
        ink: "#E6EAF2",
        dim: "#8B95A7",
        faint: "#5B6577",
        // Semantics. Green/coral are outcome; AMBER IS CONSEQUENCE — real
        // money is not "bad", it is serious, and it deserves its own hue
        // rather than borrowing the alarm colour.
        go: "#4ADE9B",
        alarm: "#F4635E",
        consequence: "#FFB347",
        brand: "#6E8BFF",
      },
      fontFamily: {
        display: ["Chakra Petch", "system-ui", "sans-serif"],
        sans: ["IBM Plex Sans", "system-ui", "sans-serif"],
        mono: ["IBM Plex Mono", "ui-monospace", "monospace"],
      },
      fontSize: {
        eyebrow: ["10px", { lineHeight: "1", letterSpacing: "0.14em" }],
      },
      keyframes: {
        pulse: { "0%,100%": { opacity: "1" }, "50%": { opacity: "0.35" } },
        draw: { from: { opacity: "0", transform: "translateX(-6px)" }, to: { opacity: "1", transform: "none" } },
      },
      animation: {
        heart: "pulse 2.4s ease-in-out infinite",
        draw: "draw .32s cubic-bezier(.2,.7,.3,1) backwards",
      },
    },
  },
};
