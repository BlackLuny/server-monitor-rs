// asciinema-player ships JS without bundled types. We rely on the small
// surface our app uses (`create`) — anything beyond that flows through
// `unknown` so a typo on options is still flagged at the call site.
declare module 'asciinema-player' {
  export interface PlayerInstance {
    dispose?: () => void;
    play?: () => void;
    pause?: () => void;
    seek?: (target: number | string) => void;
  }

  export interface CreateOptions {
    cols?: number;
    rows?: number;
    autoPlay?: boolean;
    preload?: boolean;
    loop?: boolean | number;
    startAt?: number | string;
    speed?: number;
    idleTimeLimit?: number;
    theme?: string;
    poster?: string;
    fit?: 'width' | 'height' | 'both' | 'none' | false;
    terminalFontSize?: string;
    terminalFontFamily?: string;
    terminalLineHeight?: number;
    controls?: boolean | 'auto';
  }

  export type Source =
    | string
    | { url: string; fetchOpts?: RequestInit }
    | { data: string | unknown };

  export function create(
    src: Source,
    target: HTMLElement,
    opts?: CreateOptions
  ): PlayerInstance;
}
