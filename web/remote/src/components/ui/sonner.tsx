import { useSyncExternalStore } from "react";
import { Toaster as Sonner, type ToasterProps } from "sonner";
import { getAppearance, subscribeAppearance } from "@/lib/appearance";

const Toaster = (props: ToasterProps) => {
  const theme = useSyncExternalStore(subscribeAppearance, getAppearance);
  return <Sonner theme={theme} className="toaster group" {...props} />;
};

export { Toaster };
