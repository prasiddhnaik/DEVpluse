import type { Metadata } from "next";

import "./globals.css";
import { DaemonProvider } from "@/lib/daemon";
import { AppShell } from "@/components/AppShell";

export const metadata: Metadata = {
  title: "DevPulse",
  description: "What your local code is doing, without configuring anything first.",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body className="min-h-full bg-surface text-zinc-900 antialiased dark:text-zinc-100">
        <DaemonProvider>
          <AppShell>{children}</AppShell>
        </DaemonProvider>
      </body>
    </html>
  );
}
