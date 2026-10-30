'use client'

import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { commands, type MangaDexCacheMode, type MangaDexOption } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'

export function MangaDexMultiSelect({
  label,
  options,
  value,
  onChange,
  disabled = false,
}: {
  label: string
  options: Array<{ id: string; name: string }>
  value: string[]
  onChange: (value: string[]) => void
  disabled?: boolean
}) {
  const { t } = useTranslation()
  const [text, setText] = useState('')
  const selected = options.filter((option) => value.includes(option.id))
  return (
    <div className='flex min-w-0 flex-col gap-2'>
      <p className='text-xs leading-5'>{label}</p>
      <Popover>
        <PopoverTrigger
          render={
            <Button
              type='button'
              variant='outline'
              disabled={disabled}
              aria-label={label}
              className='h-9 w-full justify-start overflow-hidden text-sm font-normal'
            />
          }
        >
          <span className='truncate'>
            {selected.length ? selected.map((option) => option.name).join(', ') : t('mangadex.all')}
          </span>
          {value.length > 0 && (
            <span className='ml-auto shrink-0 text-muted-foreground'>{value.length}</span>
          )}
        </PopoverTrigger>
        <PopoverContent align='start' className='w-72'>
          <Input
            aria-label={t('mangadex.findOption', { label })}
            placeholder={t('mangadex.findOption', { label })}
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
          <div className='max-h-60 space-y-1 overflow-y-auto' role='group' aria-label={label}>
            {options
              .filter((option) =>
                option.name.toLocaleLowerCase().includes(text.toLocaleLowerCase()),
              )
              .map((option) => (
                <label
                  key={option.id}
                  className='flex cursor-pointer items-center gap-2 rounded p-1.5 text-xs hover:bg-muted'
                >
                  <Checkbox
                    checked={value.includes(option.id)}
                    onCheckedChange={() =>
                      onChange(
                        value.includes(option.id)
                          ? value.filter((id) => id !== option.id)
                          : [...value, option.id],
                      )
                    }
                  />
                  {option.name}
                </label>
              ))}
          </div>
          <Button type='button' size='sm' variant='ghost' onClick={() => onChange([])}>
            {t('mangadex.clear')}
          </Button>
        </PopoverContent>
      </Popover>
    </div>
  )
}

export function MangaDexPeople({
  label,
  value,
  onChange,
  cache,
  disabled,
}: {
  label: string
  value: MangaDexOption[]
  onChange: (value: MangaDexOption[]) => void
  cache: MangaDexCacheMode
  disabled: boolean
}) {
  const { t } = useTranslation()
  const [name, setName] = useState('')
  const [options, setOptions] = useState<MangaDexOption[]>([])
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)
  useEffect(() => {
    let active = true
    setOptions([])
    setError('')
    setPending(false)
    if (disabled || name.trim().length < 2) return
    setPending(true)
    const timer = setTimeout(() => {
      setPending(true)
      void commands
        .getMangadexOptions('people', name.trim(), cache)
        .then((result) => {
          if (active) setOptions(result.options)
        })
        .catch((cause) => {
          if (active) setError(String(cause))
        })
        .finally(() => {
          if (active) setPending(false)
        })
    }, 450)
    return () => {
      active = false
      clearTimeout(timer)
    }
  }, [name, cache, disabled])
  return (
    <div className='min-w-0 space-y-1.5'>
      <label className='grid min-w-0 content-start gap-2 text-xs leading-5'>
        <span>{label}</span>
        <Input
          className='h-9 text-sm'
          disabled={disabled}
          value={name}
          maxLength={100}
          onChange={(event) => setName(event.target.value)}
          placeholder={t('mangadex.peopleHint')}
        />
      </label>
      {value.length > 0 && (
        <div className='flex flex-wrap gap-1'>
          {value.map((person) => (
            <Button
              key={person.id}
              type='button'
              variant='secondary'
              size='sm'
              disabled={disabled}
              aria-label={t('mangadex.remove', { name: person.name })}
              onClick={() => onChange(value.filter((item) => item.id !== person.id))}
            >
              {person.name} ×
            </Button>
          ))}
        </div>
      )}
      {pending && <p className='text-xs text-muted-foreground'>{t('mangadex.lookingUp')}</p>}
      {error && (
        <p role='alert' className='text-xs break-words text-destructive'>
          {error}
        </p>
      )}
      {options.length > 0 && (
        <div className='max-h-32 overflow-y-auto rounded border p-1' aria-label={label}>
          {options
            .filter((option) => !value.some((person) => person.id === option.id))
            .map((option) => (
              <Button
                key={option.id}
                type='button'
                variant='ghost'
                size='sm'
                className='w-full justify-start'
                onClick={() => {
                  onChange([...value, option])
                  setName('')
                }}
              >
                {option.name}
              </Button>
            ))}
        </div>
      )}
      {!pending && name.trim().length >= 2 && !error && options.length === 0 && (
        <p className='text-xs text-muted-foreground'>{t('mangadex.noPeople')}</p>
      )}
    </div>
  )
}
