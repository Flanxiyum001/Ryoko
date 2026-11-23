import { describe, expect, it } from 'vitest';
import { folderName, MODELS, validModelId } from './onboarding';

describe('first-run onboarding helpers', () => {
  it('provides sensible defaults for local and hosted model backends', () => {
    expect(MODELS.openai).toBeTruthy();
    expect(MODELS.anthropic).toBeTruthy();
    expect(MODELS.gemini).toBeTruthy();
    expect(MODELS.ollama).toBeTruthy();
  });

  it('accepts model names but rejects control characters or empty values', () => {
    expect(validModelId('gpt-4.1-mini')).toBe(true);
    expect(validModelId('qwen2.5-coder:7b')).toBe(true);
    expect(validModelId('anthropic/claude-sonnet-4-5')).toBe(true);
    expect(validModelId('../bad')).toBe(true); // model is an identifier, not a file path
    expect(validModelId('')).toBe(false);
    expect(validModelId('model\nvalue')).toBe(false);
    expect(validModelId('A'.repeat(129))).toBe(false);
  });

  it('renders Windows and POSIX path labels without exposing credentials', () => {
    expect(folderName('/home/dev/Ryoko')).toBe('Ryoko');
    expect(folderName('C:\\code\\Ryoko\\')).toBe('Ryoko');
  });
});
