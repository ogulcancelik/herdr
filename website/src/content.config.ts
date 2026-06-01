import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

function docsPath({ entry }: { entry: string }) {
  const slug = entry.replace(/\.(md|mdx|markdown|mdown|mkdn|mkd|mdwn)$/i, '');
  return slug === 'index' ? 'docs' : `docs/${slug}`;
}

export const collections = {
  docs: defineCollection({ loader: docsLoader({ generateId: docsPath }), schema: docsSchema() }),
  blog: defineCollection({
    loader: glob({ pattern: '*.md', base: './src/content/blog' }),
    schema: z.object({
      title: z.string(),
      description: z.string(),
      date: z.coerce.date(),
      draft: z.boolean().default(false),
    }),
  }),
  releases: defineCollection({
    loader: glob({ pattern: '*.md', base: './src/content/releases' }),
    schema: z.object({
      title: z.string(),
      version: z.string(),
      path: z.string(),
      description: z.string(),
      date: z.coerce.date(),
      draft: z.boolean().default(false),
    }),
  }),
};
