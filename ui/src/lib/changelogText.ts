// The repository's CHANGELOG.md, bundled at build time: the dashboard is served
// by the hub of the version it was built with, so the text is the one of the
// devstack that is running. (A new route would only hand out what the page
// already holds.)
import raw from '../../../CHANGELOG.md?raw';
import { parseChangelog } from './changelog';

export const changelog = parseChangelog(raw);
