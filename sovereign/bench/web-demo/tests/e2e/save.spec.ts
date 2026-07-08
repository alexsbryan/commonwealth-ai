import { test, expect } from '@playwright/test';

test('clicking Save shows a confirmation toast', async ({ page }) => {
  await page.goto('/');
  await page.getByLabel('Note').fill('remember the milk');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('status')).toBeVisible();
  await expect(page.getByRole('status')).toHaveText('Saved!');
});
