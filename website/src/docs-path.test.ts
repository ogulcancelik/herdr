import { describe, expect, test } from 'bun:test';
import { docsPath, docsRoute, docsTargetHref, docsVersion } from './docs-path';

const scopes = {
  stable: { locales: { root: ['', 'install'], ja: ['', 'install'] } },
  preview: { locales: { root: ['', 'install', 'new-page'], ja: ['', 'install'] } },
  '0.7.4': { locales: { root: ['', 'install'], ja: [''] } },
};

describe('docsRoute', () => {
  test.each([
    ['/docs/', { isDocs: true, locale: undefined, target: 'stable', page: '' }],
    ['/ja/docs/install/', { isDocs: true, locale: 'ja', target: 'stable', page: 'install' }],
    ['/docs/preview/new-page/', { isDocs: true, locale: undefined, target: 'preview', page: 'new-page' }],
    ['/zh-cn/docs/preview/', { isDocs: true, locale: 'zh-cn', target: 'preview', page: '' }],
    ['/docs/0.7.4/install/', { isDocs: true, locale: undefined, target: '0.7.4', page: 'install' }],
    ['/ja/docs/0.7.4/', { isDocs: true, locale: 'ja', target: '0.7.4', page: '' }],
    ['/blog/', { isDocs: false, locale: undefined, target: 'stable', page: '' }],
  ])('parses %s', (pathname, expected) => {
    expect(docsRoute(pathname)).toEqual(expected);
  });
});

describe('docsVersion', () => {
  test.each([
    ['/docs/', 'stable'],
    ['/ja/docs/preview/install/', 'preview'],
    ['/zh-cn/docs/0.7.4/', '0.7.4'],
  ])('maps %s', (pathname, version) => {
    expect(docsVersion(pathname)).toBe(version);
  });
});

describe('docsTargetHref', () => {
  test.each([
    ['/docs/install/', 'preview', '/docs/preview/install/'],
    ['/ja/docs/install/', '0.7.4', '/ja/docs/0.7.4/'],
    ['/docs/0.7.4/install/', 'stable', '/docs/install/'],
    ['/docs/preview/new-page/', '0.7.4', '/docs/0.7.4/'],
  ])('maps %s to %s', (pathname, target, expected) => {
    expect(docsTargetHref(docsRoute(pathname), target, scopes)).toBe(expected);
  });
});

describe('docsPath', () => {
  test.each([
    ['index.mdx', 'docs'],
    ['install.mdx', 'docs/install'],
    ['ja/index.mdx', 'ja/docs'],
    ['ja/install.mdx', 'ja/docs/install'],
    ['zh-cn/install.mdx', 'zh-cn/docs/install'],
    ['preview/index.mdx', 'docs/preview'],
    ['preview/install.mdx', 'docs/preview/install'],
    ['preview/ja/index.mdx', 'ja/docs/preview'],
    ['preview/ja/install.mdx', 'ja/docs/preview/install'],
    ['_versions/0.7.4/index.mdx', 'docs/0.7.4'],
    ['_versions/0.7.4/install.mdx', 'docs/0.7.4/install'],
    ['_versions/0.7.4/ja/index.mdx', 'ja/docs/0.7.4'],
    ['_versions/0.7.4/zh-cn/install.mdx', 'zh-cn/docs/0.7.4/install'],
  ])('maps %s to %s', (entry, expected) => {
    expect(docsPath({ entry })).toBe(expected);
  });
});
