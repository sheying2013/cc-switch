import { useTranslation } from "react-i18next";
import { useState } from "react";
import type { ReactNode } from "react";
import {
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from "@/components/ui/form";
import { ImeSafeInput } from "@/components/ui/ime-safe-input";
import { Button } from "@/components/ui/button";
import { ArrowLeft, Loader2, TestTube2 } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogTrigger,
  DialogClose,
} from "@/components/ui/dialog";
import { ProviderIcon } from "@/components/ProviderIcon";
import { IconPicker } from "@/components/IconPicker";
import { getIconMetadata } from "@/icons/extracted/metadata";
import type { UseFormReturn } from "react-hook-form";
import type { ProviderFormData } from "@/lib/schemas/provider";
import {
  useGlobalProxyUrl,
  useGlobalProxyChaining,
  useTestOutboundProxy,
} from "@/hooks/useGlobalProxy";

interface BasicFormFieldsProps {
  form: UseFormReturn<ProviderFormData>;
  /** Slot to render content between icon and name fields */
  beforeNameSlot?: ReactNode;
}

export function BasicFormFields({
  form,
  beforeNameSlot,
}: BasicFormFieldsProps) {
  const { t } = useTranslation();
  const [iconDialogOpen, setIconDialogOpen] = useState(false);
  const { data: globalProxyUrl } = useGlobalProxyUrl();
  const { data: isChainingEnabled } = useGlobalProxyChaining();
  const testProxyMutation = useTestOutboundProxy();

  const currentIcon = form.watch("icon");
  const currentIconColor = form.watch("iconColor");
  const providerName = form.watch("name") || "Provider";
  const effectiveIconColor =
    currentIconColor ||
    (currentIcon ? getIconMetadata(currentIcon)?.defaultColor : undefined);

  const handleIconSelect = (icon: string) => {
    const meta = getIconMetadata(icon);
    form.setValue("icon", icon);
    form.setValue("iconColor", meta?.defaultColor ?? "");
  };

  return (
    <>
      {/* 图标选择区域 - 顶部居中，可选 */}
      <div className="flex justify-center mb-6">
        <Dialog open={iconDialogOpen} onOpenChange={setIconDialogOpen}>
          <DialogTrigger asChild>
            <button
              type="button"
              className="w-20 h-20 p-3 rounded-xl border-2 border-muted hover:border-primary transition-colors cursor-pointer bg-muted/30 hover:bg-muted/50 flex items-center justify-center"
              title={
                currentIcon
                  ? t("providerIcon.clickToChange", {
                      defaultValue: "点击更换图标",
                    })
                  : t("providerIcon.clickToSelect", {
                      defaultValue: "点击选择图标",
                    })
              }
            >
              <ProviderIcon
                icon={currentIcon}
                name={providerName}
                color={effectiveIconColor}
                size={48}
              />
            </button>
          </DialogTrigger>
          <DialogContent
            variant="fullscreen"
            zIndex="top"
            overlayClassName="bg-[hsl(var(--background))] backdrop-blur-0"
            className="p-0 sm:rounded-none"
          >
            <div className="flex h-full flex-col">
              <div className="flex-shrink-0 py-4 border-b border-border-default bg-muted/40">
                <div className="px-6 flex items-center gap-4">
                  <DialogClose asChild>
                    <Button
                      type="button"
                      variant="outline"
                      size="icon"
                      aria-label={t("common.back")}
                    >
                      <ArrowLeft className="h-4 w-4" />
                    </Button>
                  </DialogClose>
                  <p className="text-lg font-semibold leading-tight">
                    {t("providerIcon.selectIcon", {
                      defaultValue: "选择图标",
                    })}
                  </p>
                </div>
              </div>
              <div className="flex-1 overflow-y-auto">
                <div className="space-y-2 px-6 py-6 w-full">
                  <IconPicker
                    value={currentIcon}
                    onValueChange={handleIconSelect}
                    color={effectiveIconColor}
                  />
                  <div className="flex justify-end gap-2">
                    <DialogClose asChild>
                      <Button type="button" variant="outline">
                        {t("common.done", { defaultValue: "完成" })}
                      </Button>
                    </DialogClose>
                  </div>
                </div>
              </div>
            </div>
          </DialogContent>
        </Dialog>
      </div>

      {/* Slot for additional fields between icon and name */}
      {beforeNameSlot}

      {/* 基础信息 - 网格布局 */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel>{t("provider.name")}</FormLabel>
              <FormControl>
                <ImeSafeInput
                  ref={field.ref}
                  name={field.name}
                  value={field.value ?? ""}
                  onValueChange={field.onChange}
                  onBlur={field.onBlur}
                  disabled={field.disabled}
                  placeholder={t("provider.namePlaceholder")}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="notes"
          render={({ field }) => (
            <FormItem>
              <FormLabel>{t("provider.notes")}</FormLabel>
              <FormControl>
                <ImeSafeInput
                  ref={field.ref}
                  name={field.name}
                  value={field.value ?? ""}
                  onValueChange={field.onChange}
                  onBlur={field.onBlur}
                  disabled={field.disabled}
                  placeholder={t("provider.notesPlaceholder")}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
      </div>

      <FormField
        control={form.control}
        name="websiteUrl"
        render={({ field }) => (
          <FormItem>
            <FormLabel>{t("provider.websiteUrl")}</FormLabel>
            <FormControl>
              <ImeSafeInput
                ref={field.ref}
                name={field.name}
                value={field.value ?? ""}
                onValueChange={field.onChange}
                onBlur={field.onBlur}
                disabled={field.disabled}
                placeholder={t("providerForm.websiteUrlPlaceholder")}
              />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="outboundProxyUrl"
        render={({ field }) => (
          <FormItem>
            <div className="flex items-center justify-between">
              <FormLabel>
                {t("provider.outboundProxyUrl", {
                  defaultValue: "单独出站代理",
                })}
              </FormLabel>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={!field.value?.trim() || testProxyMutation.isPending}
                onClick={async () => {
                  if (field.value?.trim()) {
                    await testProxyMutation.mutateAsync({
                      proxyUrl: field.value.trim(),
                      frontProxy:
                        isChainingEnabled && globalProxyUrl
                          ? globalProxyUrl
                          : null,
                    });
                  }
                }}
                className="h-6 px-2 text-xs text-muted-foreground hover:text-foreground"
              >
                {testProxyMutation.isPending ? (
                  <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />
                ) : (
                  <TestTube2 className="h-3.5 w-3.5 mr-1" />
                )}
                {t("settings.globalProxy.test", { defaultValue: "测试连接" })}
              </Button>
            </div>
            <FormControl>
              <ImeSafeInput
                ref={field.ref}
                name={field.name}
                value={field.value ?? ""}
                onValueChange={field.onChange}
                onBlur={field.onBlur}
                disabled={field.disabled}
                placeholder="http://127.0.0.1:7890 / socks5://127.0.0.1:1080"
              />
            </FormControl>
            <p className="text-xs text-muted-foreground">
              {t("provider.outboundProxyHint", {
                defaultValue:
                  "为此供应商配置单独的出站代理，支持 HTTP 和 SOCKS5。留空则使用全局出站代理或直连。",
              })}
            </p>
            <FormMessage />
          </FormItem>
        )}
      />
    </>
  );
}
