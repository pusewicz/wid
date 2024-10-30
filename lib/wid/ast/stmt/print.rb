module Wid
  module AST
    class Stmt
      class Print < Stmt
        def initialize(expressions:)
          @expressions = expressions
        end

        def to_h
          super.merge(expressions: @expressions.map(&:to_h))
        end
      end
    end
  end
end
