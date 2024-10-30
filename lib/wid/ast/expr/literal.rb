module Wid
  module AST
    class Expr
      class Literal < Expr
        attr_reader :value

        def initialize(value:)
          @value = value
        end

        def to_h
          super.merge(value: @value)
        end
      end
    end
  end
end
