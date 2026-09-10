import type { Metadata } from 'next';
import './globals.css';

export const metadata: Metadata = {
  title: 'rigor — see what you can merge',
  description:
    'A terminal dashboard for the pull requests you have out on a repo. CI status, worktree awareness, and one key to the browser.',
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
