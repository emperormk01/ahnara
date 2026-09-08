import tsParser from "@typescript-eslint/parser";
export default [{
  files: ["src/**/*.{ts,tsx}"],
  languageOptions: { parser: tsParser, parserOptions: { ecmaVersion: 2022, sourceType: "module" } },
  rules: { complexity: ["error", 10] }
}];
