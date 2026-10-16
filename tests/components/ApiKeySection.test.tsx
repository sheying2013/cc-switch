import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiKeySection } from "@/components/providers/forms/shared/ApiKeySection";

// 测试的 i18n 是空资源，这里让 t() 原样返回 key
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

function renderSection(
  props: Partial<React.ComponentProps<typeof ApiKeySection>> = {},
) {
  return render(
    <ApiKeySection
      value=""
      onChange={() => {}}
      category="third_party"
      shouldShowLink
      websiteUrl="https://example.com/keys"
      {...props}
    />,
  );
}

describe("ApiKeySection", () => {
  it("renders the API Key link when a website URL is given", () => {
    renderSection();

    expect(
      screen.getByRole("link", { name: /providerForm\.getApiKey/ }),
    ).toHaveAttribute("href", "https://example.com/keys");
  });

  it("hides the link when shouldShowLink is false", () => {
    renderSection({ shouldShowLink: false });

    expect(
      screen.queryByRole("link", { name: /providerForm\.getApiKey/ }),
    ).not.toBeInTheDocument();
  });

  it("hides the link when there is no website URL", () => {
    renderSection({ websiteUrl: "" });

    expect(
      screen.queryByRole("link", { name: /providerForm\.getApiKey/ }),
    ).not.toBeInTheDocument();
  });
});
