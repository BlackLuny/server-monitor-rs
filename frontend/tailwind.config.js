/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{html,svelte,ts,js}'],
  darkMode: 'class',
  theme: {
    extend: {
      fontFamily: {
        sans: [
          '"Geist Variable"',
          '-apple-system',
          'BlinkMacSystemFont',
          'Segoe UI',
          'system-ui',
          'sans-serif'
        ],
        mono: [
          '"JetBrains Mono Variable"',
          '"JetBrains Mono"',
          'ui-monospace',
          'Menlo',
          'Monaco',
          'Consolas',
          'monospace'
        ]
      },
      fontSize: {
        // Small, dense scale tuned for ops tools.
        '2xs': ['10px', { lineHeight: '14px', letterSpacing: '0.04em' }],
        xs: ['11px', { lineHeight: '15px' }],
        sm: ['12px', { lineHeight: '17px' }],
        base: ['13px', { lineHeight: '18px' }],
        md: ['14px', { lineHeight: '20px' }],
        lg: ['16px', { lineHeight: '22px' }],
        xl: ['20px', { lineHeight: '26px' }],
        '2xl': ['28px', { lineHeight: '32px', letterSpacing: '-0.015em' }],
        '3xl': ['36px', { lineHeight: '40px', letterSpacing: '-0.02em' }]
      },
      colors: {
        // Mirror the CSS variables so both utility classes and inline styles
        // work. Tailwind uses rgb() by default but we're on OKLCH so we just
        // forward the variable.
        base: 'var(--bg-base)',
        elev: {
          1: 'var(--bg-elev-1)',
          2: 'var(--bg-elev-2)'
        },
        recess: 'var(--bg-recess)',
        border: 'var(--border)',
        'border-strong': 'var(--border-strong)',
        'border-accent': 'var(--border-accent)',
        fg: {
          DEFAULT: 'var(--fg-primary)',
          secondary: 'var(--fg-secondary)',
          tertiary: 'var(--fg-tertiary)',
          quaternary: 'var(--fg-quaternary)'
        },
        online: 'var(--status-online)',
        warn: 'var(--status-warn)',
        error: 'var(--status-error)',
        idle: 'var(--status-idle)',
        data: {
          1: 'var(--data-1)',
          2: 'var(--data-2)',
          3: 'var(--data-3)',
          4: 'var(--data-4)',
          5: 'var(--data-5)'
        }
      },
      borderRadius: {
        none: '0',
        xs: '2px',
        sm: '4px',
        DEFAULT: '6px',
        lg: '10px'
      },
      spacing: {
        // Deliberately not a perfect 4/8 grid — ops-dense layouts benefit
        // from half-steps at small sizes and big jumps at container level.
        px: '1px',
        0.5: '2px',
        1: '4px',
        1.5: '6px',
        2: '8px',
        3: '12px',
        4: '16px',
        5: '20px',
        6: '24px',
        8: '32px',
        10: '40px',
        12: '48px',
        16: '64px',
        20: '80px'
      },
      transitionTimingFunction: {
        'out-quart': 'cubic-bezier(0.25, 1, 0.5, 1)',
        'out-expo': 'cubic-bezier(0.16, 1, 0.3, 1)'
      }
    }
  },
  plugins: []
};
