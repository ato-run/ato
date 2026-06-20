import React from 'react'

const RINGS = [0, 60, 120, 180, 240, 300]

export default function OrbitBackground({ color }) {
  return (
    <div className="orbit-container">
      <div className="orbit-wrapper">
        {RINGS.map((deg, i) => (
          <div
            key={deg}
            className={`orbit-ring orbit-ring-${i}`}
            style={{ borderColor: color }}
          />
        ))}
      </div>
    </div>
  )
}
