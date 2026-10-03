const note = document.querySelector<HTMLInputElement>('#note')!;
const saveButton = document.querySelector<HTMLButtonElement>('#save')!;
const toast = document.querySelector<HTMLDivElement>('#toast')!;

saveButton.addEventListener('click', () => {
  localStorage.setItem('note', note.value);
  toast.textContent = 'Saved!';
  toast.hidden = true;
});
