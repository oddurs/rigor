// StyleX compiles at build time. The Babel plugin rewrites stylex.create /
// stylex.props into atomic class names; @stylexjs/postcss-plugin collects the
// matching rules and substitutes them for the `@stylex;` directive in
// app/globals.css. Both must share this config or the hashes will not match.
//
// A custom Babel config puts Next on webpack rather than Turbopack, which is
// why the scripts pass --webpack explicitly.
const styleXPlugin = require('@stylexjs/babel-plugin');

module.exports = {
  presets: ['next/babel'],
  plugins: [
    [
      styleXPlugin,
      {
        runtimeInjection: false,
        dev: process.env.NODE_ENV !== 'production',
        unstable_moduleResolution: { type: 'commonJS', rootDir: __dirname },
      },
    ],
  ],
};
