/**
 * 从各种错误对象中提取错误信息
 * @param error 错误对象
 * @returns 提取的错误信息字符串
 */
export const extractErrorMessage = (error: unknown): string => {
  if (!error) return "";
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }

  if (typeof error === "object") {
    const errObject = error as Record<string, unknown>;

    const candidate = errObject.message ?? errObject.error ?? errObject.detail;
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate;
    }

    const payload = errObject.payload;
    if (typeof payload === "string" && payload.trim()) {
      return payload;
    }
    if (payload && typeof payload === "object") {
      const payloadObj = payload as Record<string, unknown>;
      const payloadCandidate =
        payloadObj.message ?? payloadObj.error ?? payloadObj.detail;
      if (typeof payloadCandidate === "string" && payloadCandidate.trim()) {
        return payloadCandidate;
      }
    }
  }

  return "";
};

export const translatePiProviderMutationError = (
  message: string,
  t: (key: string, options?: Record<string, unknown>) => string,
): string => {
  if (!message) return "";
  // Oh My Pi 的后端错误本身就是 i18n key（`omp.errors.*`）。
  const ompKey = message.match(/omp\.errors\.[A-Za-z]+/)?.[0];
  if (ompKey) return t(ompKey);

  if (
    message.includes("models.json changed") ||
    message.includes("changed outside CC Switch") ||
    message.includes("no longer present in models.json") ||
    message.includes("another value now owns the key")
  ) {
    return t("pi.provider.writeConflict");
  }

  if (message.includes("Pi provider") && message.includes("already exists")) {
    return t("pi.form.providerKeyDuplicate");
  }

  return "";
};
/**
 * Kimi 的默认模型可能在 Kimi Code 里被改过、页面上还是旧的，
 * 后端按 config.toml 现读拒绝后，把这两条英文原因换成说明。
 */
export const translateKimiProviderMutationError = (
  message: string,
  t: (key: string, options?: Record<string, unknown>) => string,
): string => {
  if (!message) return "";
  if (message.includes("Select another default model in Kimi Code")) {
    return t("kimi.error.defaultModelInUse");
  }
  if (message.includes("Update the secondary model selection in Kimi Code")) {
    return t("kimi.error.secondaryModelInUse");
  }
  return "";
};
