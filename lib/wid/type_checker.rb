# frozen_string_literal: true
# typed: strict

require_relative "type"

require "bigdecimal"

module Wid
  class TypeChecker
    extend T::Sig

    sig { params(input: Object).returns(Type) }
    def typecheck(input)
      case input
      when Integer then return Type::Int
      when Float then return Type::Float
      when BigDecimal then return Type::Double
      end

      if string?(input)
        return Type::String
      end

      raise "Unknown type for `#{input}'"
    end

    sig { params(input: Object).returns(Type) }
    def self.typecheck(input)
      new.typecheck(input)
    end

    private

    sig { params(input: Object).returns(T::Boolean) }
    def string?(input)
      return false unless input.is_a?(String)
      return input.end_with?('"') if input.start_with?('"')
      return input.end_with?("'") if input.start_with?("'")

      false
    end
  end
end
