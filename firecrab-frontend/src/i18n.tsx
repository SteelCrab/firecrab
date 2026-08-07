import { createContext, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

export type Locale = "en" | "ko";

const STORAGE_KEY = "firecrab.locale";

interface I18nValue {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  /** English first keeps call sites easy to scan and makes it the default. */
  t: (english: string, korean: string) => string;
}

const I18nContext = createContext<I18nValue | null>(null);

function initialLocale(): Locale {
  try {
    return localStorage.getItem(STORAGE_KEY) === "ko" ? "ko" : "en";
  } catch {
    return "en";
  }
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocale] = useState<Locale>(initialLocale);

  useEffect(() => {
    document.documentElement.lang = locale === "ko" ? "ko" : "en";
    try {
      localStorage.setItem(STORAGE_KEY, locale);
    } catch {
      // Language preference is optional; private browsing can reject storage.
    }
  }, [locale]);

  const value = useMemo<I18nValue>(
    () => ({ locale, setLocale, t: (english, korean) => (locale === "ko" ? korean : english) }),
    [locale],
  );
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  const value = useContext(I18nContext);
  if (!value) throw new Error("useI18n must be used inside I18nProvider");
  return value;
}
