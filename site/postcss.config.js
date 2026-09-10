module.exports = {
  plugins: {
    '@stylexjs/postcss-plugin': {
      include: ['app/**/*.{ts,tsx}'],
    },
    autoprefixer: {},
  },
};
