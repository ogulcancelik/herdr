import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';

const repoBlob = 'https://github.com/ogulcancelik/herdr/blob/master/';
const nonCanonicalDocsPath = /^\/(?:ja\/|zh-cn\/)?docs\/(?:preview|\d+\.\d+\.\d+)(?:\/|$)/;

function rewriteHerdrLinks() {
  const docsLinks = new Map([
    ['README.md', '/docs/'],
    ['./README.md', '/docs/'],
    ['CONFIGURATION.md', '/docs/configuration/'],
    ['./CONFIGURATION.md', '/docs/configuration/'],
    ['INTEGRATIONS.md', '/docs/integrations/'],
    ['./INTEGRATIONS.md', '/docs/integrations/'],
    ['SOCKET_API.md', '/docs/socket-api/'],
    ['./SOCKET_API.md', '/docs/socket-api/'],
    ['SKILL.md', '/docs/agent-skill/'],
    ['./SKILL.md', '/docs/agent-skill/'],
  ]);

  return function transform(tree) {
    walk(tree, (node) => {
      if (!node || (node.type !== 'link' && node.type !== 'definition')) return;
      if (typeof node.url !== 'string') return;

      const [path, suffix = ''] = node.url.split(/(?=[#?])/);
      const mapped = docsLinks.get(path);
      if (mapped) {
        node.url = `${mapped}${suffix}`;
        return;
      }

      const sourcePath = path.startsWith('./') ? path.slice(2) : path;
      if (
        sourcePath.startsWith('src/') ||
        sourcePath.startsWith('scripts/') ||
        sourcePath.startsWith('assets/')
      ) {
        node.url = `${repoBlob}${sourcePath}${suffix}`;
      }
    });
  };
}

function walk(node, visitor) {
  visitor(node);
  if (!node || !Array.isArray(node.children)) return;
  for (const child of node.children) walk(child, visitor);
}

export default defineConfig({
  site: 'https://herdr.dev',
  redirects: {
    '/ja': '/ja/docs/',
    '/zh-cn': '/zh-cn/docs/',
  },
  integrations: [
    sitemap({
      filter: (page) => !nonCanonicalDocsPath.test(new URL(page).pathname),
      i18n: {
        defaultLocale: 'root',
        locales: { root: 'en', ja: 'ja', 'zh-cn': 'zh-CN' },
      },
    }),
    starlight({
      title: 'herdr',
      description: 'Terminal-native agent runtime and multiplexer.',
      favicon: '/assets/favicon.png?v=14',
      defaultLocale: 'root',
      locales: {
        root: { label: 'English', lang: 'en' },
        ja: { label: '日本語', lang: 'ja' },
        'zh-cn': { label: '简体中文', lang: 'zh-CN' },
      },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/ogulcancelik/herdr',
        },
      ],
      components: {
        Banner: './src/components/Banner.astro',
        Head: './src/components/Head.astro',
        Header: './src/components/Header.astro',
        Search: './src/components/Search.astro',
        Sidebar: './src/components/Sidebar.astro',
        SiteTitle: './src/components/SiteTitle.astro',
      },
      customCss: ['./src/styles/starlight.css'],
      head: [
        {
          // First-visit locale redirect: honors browser language order, then
          // remembers the last locale the reader actually used.
          tag: 'script',
          content: `(function () {
  try {
    var KEY = 'herdr-docs-lang';
    var path = location.pathname;
    var m = path.match(/^\\/(ja|zh-cn)(?=\\/|$)/);
    var current = m ? m[1] : path.indexOf('/docs') === 0 ? 'en' : null;
    if (!current) return;
    if (!localStorage.getItem(KEY) && current === 'en') {
      var langs = navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language || ''];
      var target = null;
      for (var i = 0; i < langs.length && !target; i++) {
        var l = String(langs[i]).toLowerCase();
        if (l === 'ja' || l.indexOf('ja-') === 0) target = 'ja';
        else if (l === 'zh' || l.indexOf('zh-') === 0) target = 'zh-cn';
        else if (l.indexOf('en') === 0) break;
      }
      if (target) {
        localStorage.setItem(KEY, target);
        location.replace('/' + target + path + location.search + location.hash);
        return;
      }
    }
    localStorage.setItem(KEY, current);
  } catch (e) {}
})();`,
        },
        {
          tag: 'meta',
          attrs: { property: 'og:image', content: 'https://herdr.dev/assets/og-card-v8.png' },
        },
        { tag: 'meta', attrs: { property: 'og:image:width', content: '1200' } },
        { tag: 'meta', attrs: { property: 'og:image:height', content: '630' } },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:alt',
            content: 'Herdr documentation — One terminal. The whole herd.',
          },
        },
        {
          tag: 'meta',
          attrs: { name: 'twitter:image', content: 'https://herdr.dev/assets/og-card-v8.png' },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image:alt',
            content: 'Herdr documentation — One terminal. The whole herd.',
          },
        },
      ],
      editLink: {
        baseUrl: 'https://github.com/ogulcancelik/herdr/edit/master/',
      },
      lastUpdated: true,
      disable404Route: true,
      sidebar: [
        {
          label: 'Start here',
          translations: { ja: 'はじめに', 'zh-CN': '从这里开始' },
          items: [
            { label: 'Overview', translations: { ja: '概要', 'zh-CN': '概览' }, slug: 'docs' },
            { label: 'Install', translations: { ja: 'インストール', 'zh-CN': '安装' }, slug: 'docs/install' },
            { label: 'Quick start', translations: { ja: 'クイックスタート', 'zh-CN': '快速开始' }, slug: 'docs/quick-start' },
            { label: 'Concepts', translations: { ja: 'コンセプト', 'zh-CN': '核心概念' }, slug: 'docs/concepts' },
            { label: 'Keyboard', translations: { ja: 'キーボード', 'zh-CN': '键盘' }, slug: 'docs/keyboard' },
          ],
        },
        {
          label: 'Using Herdr',
          translations: { ja: 'Herdr を使う', 'zh-CN': '使用 Herdr' },
          items: [
            { label: 'How to work with Herdr', translations: { ja: 'Herdr での作業の進め方', 'zh-CN': '使用 Herdr 的工作方式' }, slug: 'docs/how-to-work' },
            { label: 'Agents', translations: { ja: 'エージェント', 'zh-CN': '智能体' }, slug: 'docs/agents' },
            { label: 'Agent automation', translations: { ja: 'エージェント自動化', 'zh-CN': '智能体自动化' }, slug: 'docs/agent-automation' },
            { label: 'Session state and restore', translations: { ja: 'セッション状態と復元', 'zh-CN': '会话状态与恢复' }, slug: 'docs/session-state' },
            { label: 'Persistence and remote access', translations: { ja: '永続化とリモートアクセス', 'zh-CN': '持久化与远程访问' }, slug: 'docs/persistence-remote' },
          ],
        },
        {
          label: 'Configure',
          translations: { ja: '設定する', 'zh-CN': '配置' },
          items: [
            { label: 'Configuration', translations: { ja: '設定', 'zh-CN': '配置指南' }, slug: 'docs/configuration' },
            { label: 'Config reference', translations: { ja: '設定リファレンス', 'zh-CN': '配置参考' }, slug: 'docs/config-reference' },
            { label: 'Plugins', translations: { ja: 'プラグイン', 'zh-CN': '插件' }, slug: 'docs/plugins' },
            { label: 'Marketplace', translations: { ja: 'マーケットプレイス', 'zh-CN': '插件市场' }, slug: 'docs/marketplace' },
          ],
        },
        {
          label: 'Reference',
          translations: { ja: 'リファレンス', 'zh-CN': '参考' },
          items: [
            { label: 'CLI reference', translations: { ja: 'CLI リファレンス', 'zh-CN': 'CLI 参考' }, slug: 'docs/cli-reference' },
            { label: 'Socket API', translations: { ja: 'ソケット API', 'zh-CN': 'Socket API' }, slug: 'docs/socket-api' },
            { label: 'Integrations', translations: { ja: 'インテグレーション', 'zh-CN': '集成' }, slug: 'docs/integrations' },
            { label: 'Agent skill file', translations: { ja: 'エージェントスキルファイル', 'zh-CN': '智能体技能文件' }, slug: 'docs/agent-skill' },
            { label: 'Windows beta', translations: { ja: 'Windows ベータ', 'zh-CN': 'Windows 测试版' }, slug: 'docs/windows-beta' },
          ],
        },
        {
          label: 'Help',
          translations: { ja: 'ヘルプ', 'zh-CN': '帮助' },
          items: [
            { label: 'Troubleshooting', translations: { ja: 'トラブルシューティング', 'zh-CN': '故障排除' }, slug: 'docs/troubleshooting' },
            { label: 'Next docs', translations: { ja: '次期版ドキュメント', 'zh-CN': '下一版文档' }, slug: 'docs/preview' },
          ],
        },
      ],
    }),
  ],
  markdown: {
    remarkPlugins: [rewriteHerdrLinks],
  },
});
