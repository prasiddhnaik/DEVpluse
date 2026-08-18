import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";

import "./globals.css";
import { DaemonProvider } from "@/lib/daemon";
import { AppShell } from "@/components/AppShell";

const geistSans = Geist({
  subsets: ["latin"],
  variable: "--font-geist-sans",
});

const geistMono = Geist_Mono({
  subsets: ["latin"],
  variable: "--font-geist-mono",
});

export const metadata: Metadata = {
  title: "Runscape",
  description: "What your local code is doing, without configuring anything first.",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" className={`dark ${geistSans.variable} ${geistMono.variable}`}>
      <body className={`${geistSans.className} min-h-full bg-surface text-ink antialiased`}>
        <DaemonProvider>
          <AppShell>{children}</AppShell>
        </DaemonProvider>
      </body>
    </html>
  );
}
