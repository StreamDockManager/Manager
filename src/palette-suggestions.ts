import type { ThemeMode } from './theme-style';

type RGB = [number, number, number];
const black: RGB = [0, 0, 0];
const white: RGB = [1, 1, 1];

// Decode six-digit hexadecimal RGB into normalized channel values
function rgb(hex: string): RGB {
    return [1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255) as RGB;
}

// Encode normalized RGB channels as a six-digit hexadecimal color
function hex(rgb: RGB): string {
    return (
        '#' +
        rgb
            .map((channel) =>
                Math.round(channel * 255)
                    .toString(16)
                    .padStart(2, '0'),
            )
            .join('')
    );
}

// Interpolate each RGB channel toward the target color
function mix(color: RGB, target: RGB, amount: number): RGB {
    return color.map((channel, index) => channel + (target[index] - channel) * amount) as RGB;
}

// Convert sRGB channels to linear light and calculate relative luminance
function luminance(color: RGB): number {
    const [red, green, blue] = color.map((channel) =>
        channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

// Compare relative color luminance to calculate the accessibility contrast ratio
export function contrastRatio(a: string, b: string): number {
    const first = luminance(rgb(a));
    const second = luminance(rgb(b));
    return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
}

const darkTones = {
    canvas: 0.08,
    sidebar: 0.1,
    surface: 0.13,
    'surface-raised': 0.17,
    'surface-hover': 0.21,
    border: 0.27,
    'border-hover': 0.42,
    ink: 0.97,
    muted: 0.76,
    subtle: 0.69,
};
const lightTones = {
    canvas: 0.94,
    sidebar: 0.96,
    surface: 0.99,
    'surface-raised': 0.96,
    'surface-hover': 0.92,
    border: 0.83,
    'border-hover': 0.62,
    ink: 0.13,
    muted: 0.34,
    subtle: 0.4,
};

// Derive neutral palette surfaces and contrast-adjusted accent colors from the chosen seed
export function suggestPaletteColors(
    accent: string,
    mode: ThemeMode,
    complementary: boolean,
): Record<string, string> {
    const seed = rgb(accent);
    // Invert sRGB channels to rotate a chromatic seed to its opposite hue
    const tint = complementary ? (seed.map((channel) => 1 - channel) as RGB) : seed;
    const max = Math.max(...tint);
    const min = Math.min(...tint);
    const midpoint = (max + min) / 2;
    const tones = mode === 'dark' ? darkTones : lightTones;
    // Keep surfaces close to neutral. A gray seed naturally produces neutral companions
    const colors = Object.fromEntries(
        Object.entries(tones).map(([role, lightness]) => [
            role,
            hex(
                tint.map(
                    (channel) =>
                        lightness +
                        (channel - midpoint) * Math.min(lightness, 1 - lightness) * 0.44,
                ) as RGB,
            ),
        ]),
    );
    colors['accent-soft'] = hex(mix(rgb(colors.surface), seed, mode === 'dark' ? 0.12 : 0.09));
    const backgrounds = [
        colors.canvas,
        colors.surface,
        colors['surface-raised'],
        colors['surface-hover'],
        colors['accent-soft'],
    ];
    const textTarget = mode === 'dark' ? white : black;
    let accentText = accent;
    for (
        let step = 1;
        step <= 20 && backgrounds.some((surface) => contrastRatio(accentText, surface) < 4.5);
        step++
    ) {
        accentText = hex(mix(seed, textTarget, step / 20));
    }
    return {
        ...colors,
        'accent-ink': accentText,
        'accent-hover': hex(mix(seed, black, 0.12)),
        'accent-border': hex(mix(rgb(colors.surface), rgb(accentText), 0.56)),
        focus: accentText,
    };
}
