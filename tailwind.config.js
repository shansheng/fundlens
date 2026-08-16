export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        background: 'rgb(var(--color-background-rgb) / <alpha-value>)',
        surface: 'rgb(var(--color-surface-rgb) / <alpha-value>)',
        foreground: 'rgb(var(--color-foreground-rgb) / <alpha-value>)',
        muted: 'rgb(var(--color-muted-rgb) / <alpha-value>)',
        border: 'rgb(var(--color-border-rgb) / <alpha-value>)',
        'on-primary': 'rgb(var(--color-on-primary-rgb) / <alpha-value>)',
        primary: {
          DEFAULT: 'rgb(var(--color-primary-rgb) / <alpha-value>)',
          hover: 'rgb(var(--color-primary-hover-rgb) / <alpha-value>)',
          active: 'rgb(var(--color-primary-active-rgb) / <alpha-value>)',
        },
        success: 'rgb(var(--color-success-rgb) / <alpha-value>)',
        danger: 'rgb(var(--color-danger-rgb) / <alpha-value>)',
        warning: 'rgb(var(--color-warning-rgb) / <alpha-value>)',
        gain: {
          DEFAULT: 'rgb(var(--color-gain-rgb) / <alpha-value>)',
          subtle: 'rgb(var(--color-gain-subtle-rgb) / <alpha-value>)',
        },
        loss: {
          DEFAULT: 'rgb(var(--color-loss-rgb) / <alpha-value>)',
          subtle: 'rgb(var(--color-loss-subtle-rgb) / <alpha-value>)',
        },
      },
      fontFamily: {
        sans: ['Inter', 'Noto Sans SC', '-apple-system', 'sans-serif'],
        mono: ['Geist Mono', 'Roboto Mono', 'monospace'],
      },
      borderRadius: {
        sm: 'var(--radius-sm)',
        md: 'var(--radius-md)',
        pill: 'var(--radius-pill)',
      },
      boxShadow: {
        ring: 'var(--elev-ring)',
        raised: 'var(--elev-raised)',
      },
      transitionTimingFunction: {
        standard: 'cubic-bezier(0.2, 0, 0, 1)',
      },
    },
  },
  plugins: [],
};
