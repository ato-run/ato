import type { Metadata } from 'next';
import './globals.css';

export const metadata: Metadata = {
  title: 'BYOK AI Chat',
  description: 'Bring Your Own Key AI Chat Application',
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="ja">
      <body className="antialiased">{children}</body>
    </html>
  );
}
