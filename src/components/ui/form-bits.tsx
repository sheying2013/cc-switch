import * as React from "react";
import { Dialog, DialogContent } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/** 原生勾选框 / 单选框的统一外观（index.css 的 ui-checkbox / ui-radio） */
export const CHECKBOX_CLASS = "ui-checkbox";
export const RADIO_CLASS = "ui-radio";

/** v7 对话框外壳：宽 480（或传入）、padding 24、圆角 14。 */
export function V7Dialog({
  open,
  onOpenChange,
  width = 480,
  className,
  children,
  describedBy,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  width?: number;
  className?: string;
  children: React.ReactNode;
  /** 没有 DialogDescription 时传 undefined 关掉 Radix 的提示 */
  describedBy?: string;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {open && (
        <DialogContent
          zIndex="alert"
          aria-describedby={describedBy}
          style={{ maxWidth: width }}
          className={cn(
            "max-h-[calc(100vh-48px)] gap-4 rounded-dialog border-border bg-surface p-6 text-fg-1 shadow-v7-lg sm:rounded-dialog",
            className,
          )}
        >
          {children}
        </DialogContent>
      )}
    </Dialog>
  );
}
