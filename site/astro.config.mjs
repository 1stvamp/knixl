// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

// The specs used to be the guide, at /docs/<name>/.
const specs = ['overview', 'architecture', 'reproducibility-and-taint', 'module-system', 'template-grammar', 'cli', 'oracle'];

export default defineConfig({
	site: 'https://knixl.dev',
	redirects: Object.fromEntries(specs.map((s) => [`/docs/${s}/`, `/design/${s}/`])),
	integrations: [
		starlight({
			title: 'knixl',
			description:
				'knixl generates maintainable, human-readable Nix from small amounts of opinionated KDL.',
			logo: { src: './src/assets/knixl-logo.png', replacesTitle: true },
			favicon: '/favicon.png',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/1stvamp/knixl' }],
			editLink: { baseUrl: 'https://github.com/1stvamp/knixl/edit/main/site/' },
			customCss: ['./src/styles/custom.css'],
			plugins: [starlightLinksValidator({ errorOnLocalLinks: false })],
			sidebar: [
				{ label: 'Guide', items: [{ autogenerate: { directory: 'guide' } }] },
				{ label: 'Examples', items: [{ autogenerate: { directory: 'examples' } }] },
				{
					label: 'Design specs',
					collapsed: true,
					items: [{ autogenerate: { directory: 'design' } }],
				},
				{
					label: 'Decisions (ADRs)',
					collapsed: true,
					items: [{ autogenerate: { directory: 'adr' } }],
				},
				{ label: 'Changelog', link: '/changelog/' },
			],
		}),
	],
});
