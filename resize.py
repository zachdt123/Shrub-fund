from PIL import Image

# Open the image
img = Image.open("SHRB_img.png")

# Resize image (300px width, maintain aspect ratio)
img = img.resize((300, int(img.height * 300 / img.width)), Image.LANCZOS)

# Save the resized image
img.save("SHRB_sm_img.png")

print("Image resized successfully!")
print(f"Original size: {Image.open('SHRB_img.png').size}")
print(f"New size: {img.size}")