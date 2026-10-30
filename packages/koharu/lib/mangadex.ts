import type { MangaDexSearch } from '@koharu/bridge/protocol'

export const mangaDexDefaults = (): MangaDexSearch => ({
  title: '',
  sort: '',
  reference: 'en',
  target: 'vi',
  filter: 'any',
  original_languages: [],
  translated_languages: [],
  content_ratings: ['safe', 'suggestive', 'erotica'],
  demographics: [],
  statuses: [],
  authors: [],
  artists: [],
  included_tags: [],
  excluded_tags: [],
  include_all: false,
  exclude_all: false,
  year: null,
  has_chapters: false,
  offset: 0,
  cache: 'online',
})

export const mangaDexSorts = [
  ['', 'sortNone'],
  ['relevance.desc', 'sortMatch'],
  ['latestUploadedChapter.desc', 'sortLatest'],
  ['latestUploadedChapter.asc', 'sortOldestUpload'],
  ['title.asc', 'sortTitleAsc'],
  ['title.desc', 'sortTitleDesc'],
  ['rating.desc', 'sortRatingDesc'],
  ['rating.asc', 'sortRatingAsc'],
  ['followedCount.desc', 'sortFollowsDesc'],
  ['followedCount.asc', 'sortFollowsAsc'],
  ['createdAt.desc', 'sortAddedDesc'],
  ['createdAt.asc', 'sortAddedAsc'],
  ['year.asc', 'sortYearAsc'],
  ['year.desc', 'sortYearDesc'],
] as const

// MangaDex language identifiers include site-specific variants such as zh-hk and es-la.
export const mangaDexLanguages: Array<{ id: string; fallback: string }> = [
  {
    id: 'ja',
    fallback: 'Japanese',
  },
  {
    id: 'ko',
    fallback: 'Korean',
  },
  {
    id: 'zh',
    fallback: 'Chinese (Simplified)',
  },
  {
    id: 'zh-hk',
    fallback: 'Chinese (Traditional)',
  },
  {
    id: 'en',
    fallback: 'English',
  },
  {
    id: 'af',
    fallback: 'Afrikaans',
  },
  {
    id: 'sq',
    fallback: 'Albanian',
  },
  {
    id: 'ar',
    fallback: 'Arabic',
  },
  {
    id: 'az',
    fallback: 'Azerbaijani',
  },
  {
    id: 'eu',
    fallback: 'Basque',
  },
  {
    id: 'be',
    fallback: 'Belarusian',
  },
  {
    id: 'bn',
    fallback: 'Bengali',
  },
  {
    id: 'bg',
    fallback: 'Bulgarian',
  },
  {
    id: 'my',
    fallback: 'Burmese',
  },
  {
    id: 'ca',
    fallback: 'Catalan',
  },
  {
    id: 'cv',
    fallback: 'Chuvash',
  },
  {
    id: 'hr',
    fallback: 'Croatian',
  },
  {
    id: 'cs',
    fallback: 'Czech',
  },
  {
    id: 'da',
    fallback: 'Danish',
  },
  {
    id: 'nl',
    fallback: 'Dutch',
  },
  {
    id: 'eo',
    fallback: 'Esperanto',
  },
  {
    id: 'et',
    fallback: 'Estonian',
  },
  {
    id: 'tl',
    fallback: 'Filipino',
  },
  {
    id: 'fi',
    fallback: 'Finnish',
  },
  {
    id: 'fr',
    fallback: 'French',
  },
  {
    id: 'ka',
    fallback: 'Georgian',
  },
  {
    id: 'de',
    fallback: 'German',
  },
  {
    id: 'el',
    fallback: 'Greek',
  },
  {
    id: 'he',
    fallback: 'Hebrew',
  },
  {
    id: 'hi',
    fallback: 'Hindi',
  },
  {
    id: 'hu',
    fallback: 'Hungarian',
  },
  {
    id: 'id',
    fallback: 'Indonesian',
  },
  {
    id: 'ga',
    fallback: 'Irish',
  },
  {
    id: 'it',
    fallback: 'Italian',
  },
  {
    id: 'jv',
    fallback: 'Javanese',
  },
  {
    id: 'kk',
    fallback: 'Kazakh',
  },
  {
    id: 'la',
    fallback: 'Latin',
  },
  {
    id: 'lt',
    fallback: 'Lithuanian',
  },
  {
    id: 'ms',
    fallback: 'Malay',
  },
  {
    id: 'mn',
    fallback: 'Mongolian',
  },
  {
    id: 'ne',
    fallback: 'Nepali',
  },
  {
    id: 'no',
    fallback: 'Norwegian',
  },
  {
    id: 'fa',
    fallback: 'Persian',
  },
  {
    id: 'pl',
    fallback: 'Polish',
  },
  {
    id: 'pt',
    fallback: 'Portuguese',
  },
  {
    id: 'pt-br',
    fallback: 'Portuguese (Brazil)',
  },
  {
    id: 'ro',
    fallback: 'Romanian',
  },
  {
    id: 'ru',
    fallback: 'Russian',
  },
  {
    id: 'sr',
    fallback: 'Serbian',
  },
  {
    id: 'sk',
    fallback: 'Slovak',
  },
  {
    id: 'sl',
    fallback: 'Slovenian',
  },
  {
    id: 'es',
    fallback: 'Spanish',
  },
  {
    id: 'es-la',
    fallback: 'Spanish (LATAM)',
  },
  {
    id: 'sv',
    fallback: 'Swedish',
  },
  {
    id: 'ta',
    fallback: 'Tamil',
  },
  {
    id: 'te',
    fallback: 'Telugu',
  },
  {
    id: 'th',
    fallback: 'Thai',
  },
  {
    id: 'tr',
    fallback: 'Turkish',
  },
  {
    id: 'uk',
    fallback: 'Ukrainian',
  },
  {
    id: 'ur',
    fallback: 'Urdu',
  },
  {
    id: 'uz',
    fallback: 'Uzbek',
  },
  {
    id: 'vi',
    fallback: 'Vietnamese',
  },
]
