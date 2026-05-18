import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

const repoBlob = 'https://github.com/ogulcancelik/herdr/blob/master/';

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
  integrations: [
    starlight({
      title: 'herdr',
      description: 'Terminal-native agent runtime and multiplexer.',
      favicon: '/assets/favicon.png',
      logo: {
        src: './public/assets/logo.svg',
        alt: 'herdr',
      },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/ogulcancelik/herdr',
        },
      ],
      customCss: ['./src/styles/starlight.css'],
      editLink: {
        baseUrl: 'https://github.com/ogulcancelik/herdr/edit/master/',
      },
      lastUpdated: true,
      disable404Route: true,
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Overview', slug: 'docs' },
            { label: 'Install', slug: 'docs/install' },
            { label: 'Quick start', slug: 'docs/quick-start' },
            { label: 'Concepts', slug: 'docs/concepts' },
          ],
        },
        {
          label: 'Core guides',
          items: [
            { label: 'Persistence and remote access', slug: 'docs/persistence-remote' },
            { label: 'Agents', slug: 'docs/agents' },
            { label: 'Configuration', slug: 'docs/configuration' },
            { label: 'Integrations', slug: 'docs/integrations' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI reference', slug: 'docs/cli-reference' },
            { label: 'Socket API', slug: 'docs/socket-api' },
            { label: 'Agent skill', slug: 'docs/agent-skill' },
          ],
        },
      ],
    }),
  ],
  markdown: {
    remarkPlugins: [rewriteHerdrLinks],
  },
});
